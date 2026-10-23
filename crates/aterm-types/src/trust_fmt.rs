// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Crate-internal formatting helpers that avoid runtime-argument `format_args!` for Trust verification.

use core::fmt;

/// Adapter that renders a `Debug` value through `Display`.
pub(crate) struct DebugAsDisplay<T>(pub(crate) T);

impl<T: fmt::Debug> fmt::Display for DebugAsDisplay<T> {
    #[cfg_attr(trust_verify, trust::skip)]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

/// Format `v` using its `Debug` implementation.
#[cfg_attr(trust_verify, trust::skip)]
pub(crate) fn debug_string<T: fmt::Debug>(v: T) -> String {
    DebugAsDisplay(v).to_string()
}

/// Append `v` formatted as four lowercase hex digits.
#[inline]
pub(crate) fn push_hex4(out: &mut String, v: u16) {
    #[inline]
    const fn hex_digit(n: u16) -> char {
        let n = (n & 0xf) as u8;
        let b = if n < 10 {
            b'0' + n
        } else {
            b'a'.wrapping_add(n.wrapping_sub(10))
        };
        b as char
    }
    out.push(hex_digit(v >> 12));
    out.push(hex_digit(v >> 8));
    out.push(hex_digit(v >> 4));
    out.push(hex_digit(v));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_hex4_matches_format() {
        for v in [0u16, 1, 0xf, 0x10, 0xabc, 0x1234, 0xffff] {
            let mut s = String::new();
            push_hex4(&mut s, v);
            assert_eq!(s, format!("{v:04x}"));
        }
    }
}
