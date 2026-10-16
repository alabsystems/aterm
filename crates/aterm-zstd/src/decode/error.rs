// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Why the decoder refused a stream. Every refusal reaches the reader as an
//! [`io::Error`] of kind [`io::ErrorKind::InvalidData`] carrying a
//! [`DecodeError`]; a stream that ends early is [`io::ErrorKind::UnexpectedEof`].

use std::fmt;
use std::io;

/// A frame the decoder will not decode, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum DecodeError {
    /// The four bytes where a frame starts are neither the zstd magic number
    /// nor a skippable frame's.
    BadMagic(u32),
    /// `Frame_Header_Descriptor` sets its reserved bit.
    ReservedBit,
    /// The frame needs a dictionary (`Dictionary_ID`); aterm decodes none.
    Dictionary(u32),
    /// The frame's window is past [`crate::frame::WINDOW_MAX`].
    WindowTooLarge(u64),
    /// The frame decoded to a length other than its `Frame_Content_Size`.
    ContentSize {
        /// What the header declares.
        declared: u64,
        /// What the blocks decoded to (so far, when past the declaration).
        decoded: u64,
    },
    /// The content checksum does not match the decoded content.
    Checksum {
        /// The low 32 bits of XXH64 the frame carries.
        stored: u32,
        /// The low 32 bits of XXH64 over what decoded.
        computed: u32,
    },
    /// The frame carries no content checksum, and the reader required one
    /// ([`crate::Decoder::require_checksum`]).
    NoChecksum,
    /// A block breaks a rule of the format; the text names the rule.
    Corrupt(&'static str),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadMagic(m) => write!(f, "zstd: not a frame (magic {m:#010x})"),
            Self::ReservedBit => f.write_str("zstd: frame header sets its reserved bit"),
            Self::Dictionary(id) => write!(f, "zstd: frame needs dictionary {id}; none is loaded"),
            Self::WindowTooLarge(w) => write!(
                f,
                "zstd: frame window of {w} bytes is past the {}-byte limit",
                crate::frame::WINDOW_MAX
            ),
            Self::ContentSize { declared, decoded } => write!(
                f,
                "zstd: frame declares {declared} bytes of content but decodes to {decoded}"
            ),
            Self::Checksum { stored, computed } => write!(
                f,
                "zstd: content checksum {stored:#010x} does not match the decoded {computed:#010x}"
            ),
            Self::NoChecksum => {
                f.write_str("zstd: frame carries no content checksum, and this reader requires one")
            }
            Self::Corrupt(what) => write!(f, "zstd: corrupt block: {what}"),
        }
    }
}

impl std::error::Error for DecodeError {}

impl From<DecodeError> for io::Error {
    fn from(e: DecodeError) -> Self {
        Self::new(io::ErrorKind::InvalidData, e)
    }
}
