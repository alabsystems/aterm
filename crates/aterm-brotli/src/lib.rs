// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0 AND BSD-2-Clause
//
// The format's static data (`src/dictionary.bin`, `src/tables.rs`) are Code Components
// of RFC 7932 under the Simplified BSD License; see LICENSE-BSD-2.

//! First-party Brotli (RFC 7932): a streaming decoder, and a writer of uncompressed
//! meta-block streams.
//!
//! The decoder reads the whole format: compressed meta-blocks with any number of block
//! types, prefix codes in both representations, every context mode, run-length and
//! move-to-front context maps, distance postfix and direct codes, and the static
//! dictionary under all 121 transforms; uncompressed and metadata meta-blocks; window
//! sizes 10 to 24 bits. It refuses the large-window extension (not RFC 7932) and any
//! byte after the last meta-block. Brotli has no checksum, so a stream that decodes is
//! only as trustworthy as what vouched for its bytes.
//!
//! # Example
//!
//! ```rust
//! use std::io::Read;
//!
//! let mut br = Vec::new();
//! aterm_brotli::write_uncompressed(&mut br, b"abc").unwrap();
//! let mut back = Vec::new();
//! aterm_brotli::Decoder::new(&br[..]).read_to_end(&mut back).unwrap();
//! assert_eq!(back, b"abc");
//! ```

#![forbid(unsafe_code)]

mod bits;
mod decode;
mod error;
mod prefix;
mod tables;
mod write;

pub use decode::Decoder;
pub use error::DecodeError;
pub use write::{META_BLOCK_MAX, write_uncompressed};
