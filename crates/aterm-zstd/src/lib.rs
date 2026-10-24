// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0 AND MIT
//
// `src/decode/` is a modified, decode-only subset of upstream `ruzstd` 0.8.3
// (https://github.com/KillingSpark/zstd-rs), MIT-licensed; see LICENSE-MIT.

//! First-party Zstandard (RFC 8878): a streaming decoder, a streaming encoder, a raw
//! frame writer, and the XXH64 checksum.
//!
//! # Example
//!
//! ```rust
//! use std::io::Read;
//! use aterm_zstd::frame::{self, Checksum};
//!
//! let mut out = Vec::new();
//! frame::write_raw(&mut out, b"abc", Checksum::Xxh64).unwrap();
//! assert_eq!(out[..4], 0xFD2F_B528_u32.to_le_bytes());
//! assert_eq!(out.len(), 4 + 2 + 3 + 3 + 4); // magic, header, block header, content, checksum
//!
//! let mut back = Vec::new();
//! aterm_zstd::Decoder::new(&out[..]).read_to_end(&mut back).unwrap();
//! assert_eq!(back, b"abc");
//! ```

#![forbid(unsafe_code)]

mod decode;
pub mod encode;
pub mod frame;
pub mod xxh64;

pub use decode::{DecodeError, Decoder};
pub use encode::{Encoder, compress};
