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
mod tests {
    use super::*;
    use std::io::Read as _;

    /// `member` read back through aterm's own gzip reader (the one the `tar-gz` lane unpacks
    /// with), which checks the CRC-32 and ISIZE.
    fn gunzip(member: &[u8]) -> std::io::Result<Vec<u8>> {
        let mut out = Vec::new();
        aterm_codec::inflate::stream::GzipReader::new(member).read_to_end(&mut out)?;
        Ok(out)
    }

    fn inputs() -> Vec<Vec<u8>> {
        let all: Vec<u8> = (0..=255u8).collect();
        let mut state = 0x9e37_79b9u32;
        let noise: Vec<u8> = (0..70_000)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (state >> 24) as u8
            })
            .collect();
        vec![
            Vec::new(),
            b"a".to_vec(),
            b"<!doctype html><title>x</title>\n".repeat(400),
            all.iter().cycle().take(100_000).copied().collect(),
            noise,
        ]
    }

    /// Every input round-trips through aterm's gzip reader, which verifies the trailer.
    /// NEGATIVE CONTROL: the same member with one CRC bit flipped, or one ISIZE byte
    /// changed, is refused by that reader.
    #[test]
    fn a_member_round_trips_and_a_bad_trailer_is_refused() {
        for data in inputs() {
            let member = gzip(&data);
            assert_eq!(gunzip(&member).unwrap(), data, "{} bytes", data.len());
            let n = member.len();
            let mut crc = member.clone();
            crc[n - 8] ^= 1;
            assert!(gunzip(&crc).is_err(), "a flipped CRC bit is caught");
            let mut size = member;
            size[n - 1] ^= 0x80;
            assert!(gunzip(&size).is_err(), "a changed ISIZE is caught");
        }
    }

    /// The header carries no time, name or host, and the same bytes compress to the same
    /// member every time. NEGATIVE CONTROL: a different input gives a different member.
    #[test]
    fn the_member_is_the_same_bytes_every_time() {
        let page = b"<!doctype html><p>same</p>\n".repeat(50);
        let first = gzip(&page);
        assert_eq!(&first[..10], &HEADER, "MTIME 0, no FNAME, OS 255");
        assert_eq!(first[3], 0, "no optional fields");
        assert_eq!(first, gzip(&page));
        assert_ne!(first, gzip(b"<!doctype html><p>other</p>\n"));
    }
}
