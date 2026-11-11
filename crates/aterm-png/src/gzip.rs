// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One RFC 1952 gzip member around this crate's whole-buffer DEFLATE ([`gzip`]).

/// The member header: ID1 ID2, CM = 8 (deflate), FLG = 0 (no name, comment, extra or
/// header CRC), MTIME = 0, XFL = 0, OS = 255 (unknown), so no byte of it says when or where
/// the member was made.
const HEADER: [u8; 10] = [0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 0xff];

/// `data` as one gzip member: [`HEADER`], the DEFLATE stream [`crate::squeeze`] writes
/// (zlib level 9's search, each block the smallest of dynamic, fixed and stored; its
/// parameters are constants), then the CRC-32 and ISIZE (the length mod 2^32), both
/// little-endian.
///
/// The same input gives the same bytes on every host and every run: nothing in the member
/// depends on a clock, a file name or the machine, which makes its SHA-256 a content address.
#[must_use]
pub fn gzip(data: &[u8]) -> Vec<u8> {
    let deflate = crate::squeeze::deflate(data);
    let mut member = Vec::with_capacity(HEADER.len() + deflate.len() + 8);
    member.extend_from_slice(&HEADER);
    member.extend_from_slice(&deflate);
    member.extend_from_slice(&aterm_codec::crc32::Crc32::of(data).to_le_bytes());
    // ISIZE is the input length modulo 2^32 (RFC 1952 §2.3.1): the truncation is the format.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "RFC 1952's ISIZE is the length modulo 2^32"
    )]
    let isize = data.len() as u32;
    member.extend_from_slice(&isize.to_le_bytes());
    member
}

#[cfg(test)]
#[path = "gzip_tests.rs"]
mod tests;
