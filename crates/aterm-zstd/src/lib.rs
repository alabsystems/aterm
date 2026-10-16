// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0 AND MIT
//
// `src/decode/` is a modified, decode-only subset of upstream `ruzstd` 0.8.3
// (https://github.com/KillingSpark/zstd-rs), MIT-licensed; see LICENSE-MIT.

//! First-party Zstandard (RFC 8878) for aterm.
//!
//! aterm reads `.tar.zst` in exactly one place, atpkg's installer, and every
//! test that feeds that installer needs a valid frame. This crate replaced the
//! `zstd` / `zstd-safe` / `zstd-sys` family (libzstd's C behind a cc build
//! script) in steps ZC.3-ZC.6 (docs/AUDIT-third-party-2026-10-15.md, unit
//! zstd-c). It holds:
//!
//! * [`Decoder`]: a [`std::io::Read`] adapter over a stream of frames, the
//!   shape of `zstd::Decoder`. Carved from ruzstd 0.8.3's decode core, with a
//!   bounds-checked window in place of its `unsafe` ring buffer, the content
//!   checksum verified, and memory capped by [`frame::WINDOW_MAX`]. Its docs
//!   say exactly what it accepts.
//! * [`xxh64`]: XXH64, the hash a frame's optional content checksum is cut
//!   from. Bit-exact with the xxHash reference C code that libzstd bundles.
//! * [`frame::write_raw`]: a frame writer that stores, not compresses: one
//!   single-segment frame carrying its content size, the content in raw
//!   blocks of at most [`frame::BLOCK_MAX`] bytes, and optionally the
//!   checksum. Test fixtures need a valid frame, never a small one, and a
//!   writer this plain is easy to check (`zstd -19` stays the producer of
//!   anything shipped).
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
pub mod frame;
pub mod xxh64;

pub use decode::{DecodeError, Decoder};
