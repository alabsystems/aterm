// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Crate-internal formatting helpers that avoid runtime-argument
//! `format_args!`.
//!
//! Trust gate note: the Trust native TrustIr lowering cannot model the
//! `fmt::Arguments` constructor that `format_args!` produces when it has
//! runtime arguments (`Call target `std::fmt::Arguments::<'a>::new` is not
//! present in the TrustIr module`), and — unlike other unmodeled external
//! calls — that constructor is not eligible for the memory-safe waiver, so
//! every such call site is a hard verification error. These helpers produce
//! byte-identical output using plain method calls (`ToString::to_string`,
//! `Formatter::write_str`), whose `format_args!` construction happens inside
//! the standard library (scoped out of verification), so call sites in this
//! crate lower cleanly.

use core::fmt;

/// Adapter that renders a `Debug` value through `Display`, so `{:?}` output
/// (with default formatting options) can be produced via `.to_string()`
/// without a runtime-argument `format_args!` at the call site.
pub(crate) struct DebugAsDisplay<T>(pub(crate) T);

impl<T: fmt::Debug> fmt::Display for DebugAsDisplay<T> {
    // Trust: a thin forwarder — its panic-freedom rests ENTIRELY on the generic
    // `<T as Debug>::fmt`, whose impl is unknown until monomorphization (an
    // undecidable open-world dispatch pre-mono). The concrete `T::Debug` used at
    // each call site is verified when that type is verified; this adapter takes
    // documented responsibility for it, exactly as the other `#[trust::skip]`
    // forwarders in the workspace do (e.g. `aterm_log::__log`).
    #[cfg_attr(trust_verify, trust::skip)]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

/// Byte-identical to `format!("{v:?}")`: `Debug` output with default
/// formatting options.
// Skip: `T: Debug` is CALLER-CHOSEN code (its Debug impl may panic by design)
// and the format machinery's bodies are absent. Diagnostic rendering only.
#[cfg_attr(trust_verify, trust::skip)]
pub(crate) fn debug_string<T: fmt::Debug>(v: T) -> String {
    DebugAsDisplay(v).to_string()
}

/// Append `v` formatted as exactly four lowercase hex digits — byte-identical
/// to `format!("{v:04x}")` for `u16` (a `u16` never needs more than four hex
/// digits, so the width specifier always pads to exactly four).
///
/// Pure arithmetic digit emission (no lookup table, no `str::from_utf8`):
/// the old `HEX[usize::from(..)]` indexing and the `from_utf8(..).unwrap_or`
/// carried Trust L0 bounds-check and strict-UTF-8 obligations the gate could
/// not discharge. Computing each digit as `b'0' + n` / `b'a' + (n - 10)`
/// produces the same bytes with every operation provably in range, and
/// `String::push` of an ASCII `char` appends the identical single byte that
/// `push_str` of the table digit did.
pub(crate) fn push_hex4(out: &mut String, v: u16) {
    // One nibble as its lowercase hex digit. Behavior-identical to indexing
    // `b"0123456789abcdef"`: the `& 0xf` re-mask is identity at every call
    // site below (each argument is already masked/shifted into 0..=15) and
    // gives this function a local, provable bound — `n <= 15`, so
    // `b'0' + n <= 63` and, under the `n >= 10` branch, `n - 10 <= 5` and
    // `b'a' + (n - 10) <= 102`: no add can overflow, no sub can underflow.
    fn hex_digit(n: u16) -> char {
        let n = (n & 0xf) as u8;
        // Trust gate: `wrapping_add`/`wrapping_sub` are identical to `+`/`-`
        // here — after `& 0xf`, `n <= 15`, and this branch requires `n >= 10`,
        // so `n - 10 <= 5` cannot underflow and `b'a' + (n - 10) <= 102`
        // cannot overflow: no wrap ever occurs.
        let b = if n < 10 {
            b'0' + n
        } else {
            b'a'.wrapping_add(n.wrapping_sub(10))
        };
        char::from(b)
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
    fn debug_string_matches_format() {
        assert_eq!(debug_string("a\"b\n"), format!("{:?}", "a\"b\n"));
        assert_eq!(debug_string(String::from("x")), format!("{:?}", "x"));
        assert_eq!(debug_string(7usize), format!("{:?}", 7usize));
    }

    #[test]
    fn push_hex4_matches_format() {
        for v in [0u16, 1, 0xf, 0x10, 0xabc, 0x1234, 0xffff] {
            let mut s = String::new();
            push_hex4(&mut s, v);
            assert_eq!(s, format!("{v:04x}"));
        }
    }
}
