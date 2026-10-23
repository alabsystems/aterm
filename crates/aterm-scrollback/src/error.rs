// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Shared error type for scrollback access and storage failures.

use std::fmt;

/// Errors that can occur when accessing scrollback data.
#[non_exhaustive]
#[derive(Debug)]
pub enum ScrollbackError {
    /// I/O-shaped failure when reading from cold tier storage.
    Io(std::io::Error),
    /// Decompression failure (corrupted or invalid compressed data).
    Decompression(String),
    /// Block permanently quarantined after repeated decompression failures.
    Quarantined(usize),
    /// Memory budget enforcement failed after eviction attempts.
    EnforcementFailed {
        /// Bytes exceeding the budget after all eviction attempts.
        over_bytes: usize,
    },
}

/// Renders `v` as decimal without `format_args!` or loops so the Trust lane can lower it.
pub(crate) fn dec_string(v: usize) -> String {
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
    let _ = started;
    out.push(char::from(b'0'.wrapping_add(rem as u8)));
    out
}

impl fmt::Display for ScrollbackError {
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
}
