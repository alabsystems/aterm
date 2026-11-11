// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
