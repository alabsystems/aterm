// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Why a stream is not decoded.

use std::fmt;
use std::io;

/// A stream the decoder will not decode, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum DecodeError {
    /// The input ended inside the stream.
    Truncated,
    /// The stream asks for the large-window extension, which is not RFC 7932.
    LargeWindow,
    /// Bytes follow the stream's last meta-block.
    TrailingBytes,
    /// The stream breaks a rule of the format; the text names the rule.
    Corrupt(&'static str),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => f.write_str("brotli: the input ends inside the stream"),
            Self::LargeWindow => f.write_str(
                "brotli: the stream uses the large-window extension, which is not RFC 7932",
            ),
            Self::TrailingBytes => f.write_str("brotli: bytes follow the stream's last meta-block"),
            Self::Corrupt(what) => write!(f, "brotli: corrupt stream: {what}"),
        }
    }
}

impl std::error::Error for DecodeError {}

impl From<DecodeError> for io::Error {
    fn from(e: DecodeError) -> Self {
        let kind = if e == DecodeError::Truncated {
            io::ErrorKind::UnexpectedEof
        } else {
            io::ErrorKind::InvalidData
        };
        Self::new(kind, e)
    }
}

/// What a decoding step can fail with: the stream, or the reader under it.
#[derive(Debug)]
pub(crate) enum Fault {
    /// The stream.
    Stream(DecodeError),
    /// The reader.
    Io(io::Error),
}

impl From<DecodeError> for Fault {
    fn from(e: DecodeError) -> Self {
        Self::Stream(e)
    }
}

impl From<Fault> for io::Error {
    fn from(f: Fault) -> Self {
        match f {
            Fault::Stream(e) => e.into(),
            Fault::Io(e) => e,
        }
    }
}

/// A step's result.
pub(crate) type Step<T> = Result<T, Fault>;

/// A corrupt stream, naming the rule.
pub(crate) const fn corrupt<T>(rule: &'static str) -> Step<T> {
    Err(Fault::Stream(DecodeError::Corrupt(rule)))
}
