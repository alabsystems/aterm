// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Shared error type for scrollback access and storage failures.

use std::fmt;

/// Errors that can occur when accessing scrollback data.
///
/// Distinguishes between index-out-of-bounds (`Ok(None)`) and actual failures
/// such as I/O or decompression errors.
#[non_exhaustive]
#[derive(Debug)]
pub enum ScrollbackError {
    /// Disk I/O failure when reading from cold tier storage.
    ///
    /// Display: `scrollback I/O error: {0}`
    Io(std::io::Error),
    /// Decompression failure (corrupted or invalid compressed data).
    ///
    /// Display: `scrollback decompression error: {0}`
    Decompression(String),
    /// Block permanently quarantined after repeated decompression failures.
    ///
    /// Display: `scrollback block quarantined: {0} lines inaccessible`
    Quarantined(usize),
    /// Memory budget enforcement failed after eviction attempts.
    ///
    /// Display: `memory budget enforcement failed: {over_bytes} bytes over budget`
    EnforcementFailed {
        /// Bytes exceeding the budget after all eviction attempts.
        over_bytes: usize,
    },
}

/// Render `v` as decimal into a fresh `String` without going through
/// `format_args!`.
///
/// The `format!`/`write!` expansion places `std::fmt::Arguments` construction
/// (an unsafe, unlowerable-to-TrustIr construct) into the calling function's
/// span, which the strict Trust gate fails closed on. This helper produces the
/// exact same digits `usize`'s `Display` would (base 10, no separators, `"0"`
/// for zero).
///
/// Deliberately LOOP-FREE, digit-by-constant-power-of-ten: an earlier spelling
/// with the classic `v % 10` / `v /= 10` loop sent the strict gate's integer
/// engine into a non-terminating solve (loop-carried division; observed 40+
/// CPU-minutes before being killed). Twenty straight-line constant divisions
/// carry no loop invariant to infer and no panic obligations at all (constant
/// nonzero divisors, wrapping add of a digit that is 0..=9 by construction).
pub(crate) fn dec_string(v: usize) -> String {
    // u64 arithmetic so the 10^19 constant exists on every target width
    // (usize -> u64 is lossless on all supported targets).
    let mut rem = v as u64;
    let mut out = String::new();
    let mut started = false;
    macro_rules! emit_digit {
        ($p:expr) => {
            let d = (rem / $p) as u8;
            rem %= $p;
            if started || d != 0 {
                started = true;
                out.push(char::from(b'0'.wrapping_add(d)));
            }
        };
    }
    emit_digit!(10_000_000_000_000_000_000u64);
    emit_digit!(1_000_000_000_000_000_000u64);
    emit_digit!(100_000_000_000_000_000u64);
    emit_digit!(10_000_000_000_000_000u64);
    emit_digit!(1_000_000_000_000_000u64);
    emit_digit!(100_000_000_000_000u64);
    emit_digit!(10_000_000_000_000u64);
    emit_digit!(1_000_000_000_000u64);
    emit_digit!(100_000_000_000u64);
    emit_digit!(10_000_000_000u64);
    emit_digit!(1_000_000_000u64);
    emit_digit!(100_000_000u64);
    emit_digit!(10_000_000u64);
    emit_digit!(1_000_000u64);
    emit_digit!(100_000u64);
    emit_digit!(10_000u64);
    emit_digit!(1_000u64);
    emit_digit!(100u64);
    emit_digit!(10u64);
    // Ones digit is emitted unconditionally, so `0` renders as "0".
    let _ = started;
    out.push(char::from(b'0'.wrapping_add(rem as u8)));
    out
}

// Manual `Display`/`Error`/`From` impls (previously `derive(aterm_error::Error)`).
//
// The derive's Display arms expand `write!(f, "... {0}", ...)`, whose
// `format_args!` expansion embeds an unsafe `fmt::Arguments` construction the
// strict Trust gate cannot lower and therefore fails closed on. These impls
// produce byte-identical Display output, the identical `source()` chain, and
// the identical `From<std::io::Error>` conversion, using only `write_str` and
// direct `Display::fmt` delegation (an opaque call, no local `Arguments`).
impl fmt::Display for ScrollbackError {
    // Skip: thin generic forwarder into the wrapped error's Display (user
    // code, may panic by design) — the DebugAsDisplay class.
    #[cfg_attr(trust_verify, trust::skip)]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => {
                f.write_str("scrollback I/O error: ")?;
                fmt::Display::fmt(e, f)
            }
            Self::Decompression(msg) => {
                f.write_str("scrollback decompression error: ")?;
                f.write_str(msg)
            }
            Self::Quarantined(lines) => {
                f.write_str("scrollback block quarantined: ")?;
                f.write_str(&dec_string(*lines))?;
                f.write_str(" lines inaccessible")
            }
            Self::EnforcementFailed { over_bytes } => {
                f.write_str("memory budget enforcement failed: ")?;
                f.write_str(&dec_string(*over_bytes))?;
                f.write_str(" bytes over budget")
            }
        }
    }
}

impl std::error::Error for ScrollbackError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for ScrollbackError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `dec_string` must render exactly like `usize`'s `Display` — the manual
    /// (format_args-free) Display impls in this crate depend on it.
    #[test]
    fn dec_string_matches_display() {
        for v in [
            0usize,
            1,
            9,
            10,
            11,
            99,
            100,
            12345,
            65_535,
            1_000_000,
            usize::MAX - 1,
            usize::MAX,
        ] {
            assert_eq!(dec_string(v), format!("{v}"));
        }
    }

    /// The manual `Display` impl must render byte-identically to the strings
    /// the previous `derive(aterm_error::Error)` produced.
    #[test]
    fn display_messages_are_stable() {
        let io = ScrollbackError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "missing page",
        ));
        assert_eq!(format!("{io}"), "scrollback I/O error: missing page");
        let de = ScrollbackError::Decompression("bad frame".to_string());
        assert_eq!(format!("{de}"), "scrollback decompression error: bad frame");
        let q = ScrollbackError::Quarantined(42);
        assert_eq!(
            format!("{q}"),
            "scrollback block quarantined: 42 lines inaccessible"
        );
        let ef = ScrollbackError::EnforcementFailed { over_bytes: 1024 };
        assert_eq!(
            format!("{ef}"),
            "memory budget enforcement failed: 1024 bytes over budget"
        );
    }

    /// `source()` must expose the io error (the `#[from]` field), like the
    /// derive did.
    #[test]
    fn io_source_is_preserved() {
        use std::error::Error as _;
        let io = ScrollbackError::Io(std::io::Error::other("x"));
        assert!(io.source().is_some());
        assert!(ScrollbackError::Quarantined(1).source().is_none());
    }
}
