// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The one error type every part of the crate returns.

use std::fmt;
use std::io;

/// What went wrong. A decoder fault names the rule the bytes broke and where,
/// so a bad message from the bus is diagnosable from the error alone.
#[derive(Debug)]
pub enum Error {
    /// The socket failed.
    Io(io::Error),
    /// Bytes that break the D-Bus specification: `what` names the rule, `at`
    /// the byte offset (from the start of the message, or of the buffer handed
    /// to a decoder).
    Malformed { what: &'static str, at: usize },
    /// A value, name, address or message built on this side that the
    /// specification forbids.
    Invalid(&'static str),
    /// Valid D-Bus this crate does not speak: a unix descriptor, a transport
    /// other than a unix socket, an unknown message type.
    Unsupported(&'static str),
    /// The server refused authentication or answered out of protocol.
    Auth(String),
    /// The peer answered a call with an error message.
    Remote { name: String, message: String },
    /// No answer before the deadline.
    Timeout,
    /// The peer hung up, or an earlier framing fault left the stream unusable.
    Closed,
}

/// The crate's result.
pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    #[inline]
    pub(crate) const fn malformed(what: &'static str, at: usize) -> Self {
        Self::Malformed { what, at }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "d-bus socket: {e}"),
            Self::Malformed { what, at } => write!(f, "malformed d-bus data at byte {at}: {what}"),
            Self::Invalid(what) => write!(f, "invalid d-bus input: {what}"),
            Self::Unsupported(what) => write!(f, "unsupported d-bus feature: {what}"),
            Self::Auth(why) => write!(f, "d-bus authentication: {why}"),
            Self::Remote { name, message } if message.is_empty() => write!(f, "{name}"),
            Self::Remote { name, message } => write!(f, "{name}: {message}"),
            Self::Timeout => f.write_str("d-bus: no answer before the deadline"),
            Self::Closed => f.write_str("d-bus: connection closed"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
