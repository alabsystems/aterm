// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! JSON for aterm: a serde `Deserializer`/`Serializer` for typed models, and an untyped [`Value`] tree with `json!`, over one parser and one writer.
//!
//! ```
//! # use serde::Deserialize;
//! #[derive(Deserialize)]
//! struct Release { tag_name: String }
//!
//! // Typed.
//! let release: Release = aterm_json::from_str(r#"{"tag_name":"v1.2.3"}"#).unwrap();
//! assert_eq!(release.tag_name, "v1.2.3");
//!
//! // Untyped.
//! let body = aterm_json::json!({"model": "qwen", "options": {"temperature": 0}});
//! assert_eq!(aterm_json::to_string(&body).unwrap(),
//!            r#"{"model":"qwen","options":{"temperature":0}}"#);
//! ```

#![warn(missing_docs)]

pub mod de;
pub mod error;
mod macros;
pub mod ser;
mod value;

pub use de::{Deserializer, from_slice, from_str};
pub use error::{Error, Result};
pub use ser::{to_string, to_vec};
pub use value::{Map, Number, Value};

use serde::Deserialize as _;
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Convert any `Serialize` value into a [`Value`].
/// # Errors
/// Whatever the `Serialize` impl reports — a map with a non-string key, most commonly.
pub fn to_value<T: Serialize>(value: T) -> Result<Value> {
    let text = to_vec(&value)?;
    let mut de = de::Deserializer::exact(&text);
    let parsed = Value::deserialize(&mut de)?;
    de.end()?;
    Ok(parsed)
}

/// Interpret a [`Value`] as any `Deserialize` type.
/// # Errors
/// Whatever the `Deserialize` impl reports — a missing field or a type mismatch.
pub fn from_value<T: DeserializeOwned>(value: Value) -> Result<T> {
    let text = to_vec(&value)?;
    from_slice(&text)
}

/// Return the maximum container nesting depth enforced by the deserializer (`127`).
#[inline]
#[must_use]
pub const fn recursion_limit() -> usize {
    127
}

/// Return `true` when `depth` is within the JSON container recursion limit (`<= 127`).
#[inline]
#[must_use]
pub const fn within_recursion_limit(depth: usize) -> bool {
    depth <= 127
}

/// Return `true` when `b` is RFC 8259 JSON whitespace (`' '`, `'\t'`, `'\n'`, `'\r'`).
#[inline]
#[must_use]
pub const fn is_json_whitespace_byte(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

/// Return `true` when `b` is an unescaped RFC 8259 control byte (`0x00..=0x1F`).
#[inline]
#[must_use]
pub const fn is_json_control_byte(b: u8) -> bool {
    b <= 0x1f
}

/// Return `true` when `code` is a UTF-16 high surrogate (`0xD800..=0xDBFF`).
#[inline]
#[must_use]
pub const fn is_utf16_high_surrogate(code: u16) -> bool {
    code >= 0xd800 && code <= 0xdbff
}

/// Return `true` when `code` is a UTF-16 low surrogate (`0xDC00..=0xDFFF`).
#[inline]
#[must_use]
pub const fn is_utf16_low_surrogate(code: u16) -> bool {
    code >= 0xdc00 && code <= 0xdfff
}

/// Combine a valid UTF-16 surrogate pair (`high` in `0xD800..=0xDBFF`, `low` in `0xDC00..=0xDFFF`) into its Unicode scalar value (`0x10000..=0x10FFFF`).
#[inline]
#[must_use]
pub const fn decode_surrogate_pair(high: u16, low: u16) -> Option<u32> {
    if !is_utf16_high_surrogate(high) || !is_utf16_low_surrogate(low) {
        return None;
    }
    let hi = (high - 0xd800) as u32;
    let lo = (low - 0xdc00) as u32;
    Some(0x10000 + ((hi << 10) | lo))
}

/// Encode a JSON token span (`offset`, `len`, `line`, `col`) into 16 little-endian bytes.
#[inline]
#[must_use]
pub const fn encode_json_span_le(offset: u32, len: u32, line: u32, col: u32) -> [u8; 16] {
    [
        offset as u8,
        (offset >> 8) as u8,
        (offset >> 16) as u8,
        (offset >> 24) as u8,
        len as u8,
        (len >> 8) as u8,
        (len >> 16) as u8,
        (len >> 24) as u8,
        line as u8,
        (line >> 8) as u8,
        (line >> 16) as u8,
        (line >> 24) as u8,
        col as u8,
        (col >> 8) as u8,
        (col >> 16) as u8,
        (col >> 24) as u8,
    ]
}

/// Decode 16 little-endian bytes into a JSON token span (`offset`, `len`, `line`, `col`).
#[inline]
#[must_use]
pub const fn decode_json_span_le(bytes: [u8; 16]) -> (u32, u32, u32, u32) {
    let offset = (bytes[0] as u32)
        | ((bytes[1] as u32) << 8)
        | ((bytes[2] as u32) << 16)
        | ((bytes[3] as u32) << 24);
    let len = (bytes[4] as u32)
        | ((bytes[5] as u32) << 8)
        | ((bytes[6] as u32) << 16)
        | ((bytes[7] as u32) << 24);
    let line = (bytes[8] as u32)
        | ((bytes[9] as u32) << 8)
        | ((bytes[10] as u32) << 16)
        | ((bytes[11] as u32) << 24);
    let col = (bytes[12] as u32)
        | ((bytes[13] as u32) << 8)
        | ((bytes[14] as u32) << 16)
        | ((bytes[15] as u32) << 24);
    (offset, len, line, col)
}

#[cfg(test)]
mod const_tests {
    use super::*;

    #[test]
    fn json_const_helpers_round_trip() {
        assert_eq!(recursion_limit(), 127);
        assert!(within_recursion_limit(127));
        assert!(!within_recursion_limit(128));
        assert!(is_json_whitespace_byte(b' '));
        assert!(!is_json_whitespace_byte(b'a'));
        assert!(is_json_control_byte(0x1f));
        assert!(!is_json_control_byte(0x20));
        assert_eq!(decode_surrogate_pair(0xd83d, 0xde00), Some(0x1f600));
        assert_eq!(decode_surrogate_pair(0xd800, 0x0041), None);
        let enc = encode_json_span_le(12, 34, 5, 9);
        assert_eq!(decode_json_span_le(enc), (12, 34, 5, 9));
    }
}
