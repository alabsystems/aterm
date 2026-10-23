// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Base64, hex, percent, JSON, terminal-safe text, field-sink, CRC-32, and DEFLATE codecs.
//!
//! # Examples
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
/// Cell wire attribute vocabulary and strict style reader.
pub mod cell;
pub mod crc32;
pub mod fields;
pub mod hex;
pub mod inflate;
pub mod json;
pub mod pct;
pub mod tty;

use std::fmt;

/// Writes `v` in decimal to `f` without `format_args!`.
#[inline]
pub(crate) fn write_usize_decimal(f: &mut fmt::Formatter<'_>, v: usize) -> fmt::Result {
    let mut buf = [0u8; 20];
    let mut v = v;
    let mut i = buf.len();
    while i > 0 {
        i -= 1;
        if let Some(slot) = buf.get_mut(i) {
            *slot = b'0'.saturating_add((v % 10) as u8);
        }
        v /= 10;
        if v == 0 {
            break;
        }
    }
    match std::str::from_utf8(buf.get(i..).unwrap_or(&[])) {
        Ok(s) => f.write_str(s),
        Err(_) => Err(fmt::Error),
    }
}

/// Writes `b` as two uppercase hex digits to `f` without `format_args!`.
#[inline]
pub(crate) fn write_hex_byte_upper(f: &mut fmt::Formatter<'_>, b: u8) -> fmt::Result {
    const HEX_UPPER: &[u8; 16] = b"0123456789ABCDEF";
    let digits = [HEX_UPPER[(b >> 4) as usize], HEX_UPPER[(b & 0x0F) as usize]];
    match std::str::from_utf8(&digits) {
        Ok(s) => f.write_str(s),
        Err(_) => Err(fmt::Error),
    }
}

/// Maximum length in bytes of untrusted input accepted by any codec entry point (64 MiB).
pub const MAX_INPUT_LEN: usize = 64 << 20;
