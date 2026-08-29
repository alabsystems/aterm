// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The one error type for parsing, deserializing and serializing.
//!
//! `serde_json` splits its failure modes into categories (`Io`, `Syntax`,
//! `Data`, `Eof`) that every consumer in this tree collapses back to
//! `error.to_string()` at the call site. One type, carrying the message and the
//! 1-based line/column it was raised at, keeps that surface without the split.

use core::fmt;

/// A JSON parse, deserialize or serialize failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    message: String,
    /// 1-based position in the source, or `None` for a serializer failure and
    /// for errors raised by a `Deserialize` impl against an in-memory
    /// [`Value`](crate::Value).
    line_col: Option<(usize, usize)>,
}

impl Error {
    /// A failure with no source position.
    pub(crate) fn message(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            line_col: None,
        }
    }

    /// A failure anchored at byte `at` of `source`.
    pub(crate) fn at(source: &[u8], at: usize, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            line_col: Some(line_col(source, at)),
        }
    }

    /// The 1-based line the failure was raised on, or 0 if it has no position.
    #[must_use]
    pub fn line(&self) -> usize {
        self.line_col.map_or(0, |(line, _)| line)
    }

    /// The 1-based column the failure was raised at, or 0 if it has no position.
    #[must_use]
    pub fn column(&self) -> usize {
        self.line_col.map_or(0, |(_, col)| col)
    }
}

/// Resolve a byte offset to a 1-based line and column, counting a column per
/// CHARACTER rather than per byte so a diagnostic against a line with
/// non-ASCII text points where a reader would look.
fn line_col(source: &[u8], at: usize) -> (usize, usize) {
    let mut line = 1usize;
    let mut col = 1usize;
    let head = source.get(..at.min(source.len())).unwrap_or(source);
    for &byte in head {
        if byte == b'\n' {
            line = line.saturating_add(1);
            col = 1;
        } else if byte & 0xC0 != 0x80 {
            // Not a UTF-8 continuation byte, so it starts a character.
            col = col.saturating_add(1);
        }
    }
    (line, col)
}

impl fmt::Display for Error {
    // `write_str` + a digit writer rather than `write!`: the `format_args!`
    // expansion calls the unsafe `fmt::Arguments` constructor, which the Trust
    // model fails closed on. Same text either way.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)?;
        if let Some((line, col)) = self.line_col {
            f.write_str(" at line ")?;
            write_usize(f, line)?;
            f.write_str(" column ")?;
            write_usize(f, col)?;
        }
        Ok(())
    }
}

fn write_usize(f: &mut fmt::Formatter<'_>, v: usize) -> fmt::Result {
    // usize is at most 64 bits, so at most 20 decimal digits.
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
    match core::str::from_utf8(buf.get(i..).unwrap_or(&[])) {
        Ok(s) => f.write_str(s),
        Err(_) => Err(fmt::Error),
    }
}

impl std::error::Error for Error {}

impl serde::de::Error for Error {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        Self::message(msg.to_string())
    }
}

impl serde::ser::Error for Error {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        Self::message(msg.to_string())
    }
}

/// The crate's result alias.
pub type Result<T> = core::result::Result<T, Error>;
