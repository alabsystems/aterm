// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `aterm_codec::inflate::stream` against this crate's own one-shot `inflate`:
//! one engine, two drivers.
//!
//! atpkg's `tar-gz` and `zip` lanes hand a decompressed stream straight to a
//! walker that writes files to disk, so the properties that matter are "the
//! same bytes come out" and "a malformed input is refused". The interesting
//! risk in a streaming decoder is RESUMPTION: a unit of work interrupted by the
//! input running out has to restart from exactly where it began, so every
//! vector is read at granularities down to one byte, from a source that yields
//! one byte per `read`.

use std::io::Read;

use aterm_codec::inflate::stream::{DeflateReader, GzipReader, ZlibReader};
use aterm_codec::inflate::{inflate, zlib_decompress_prefix};

// Raw DEFLATE vectors (zlib framing stripped), produced by python3 `zlib`.
/// Fixed-Huffman literals.
const HELLO: &[u8] = &[
    0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0xd7, 0x51, 0x28, 0xcf, 0x2f, 0xca, 0x49, 0x01, 0x00,
];
/// Fixed-Huffman back-references.
const BACKREFS: &[u8] = &[0x4b, 0x4c, 0x4a, 0xa4, 0x2a, 0x04, 0x00];
/// A dynamic-Huffman block (~1.6 KB of skewed text at level 9).
const DYNAMIC: &[u8] = &[
    0xed, 0xce, 0x41, 0x12, 0x02, 0x21, 0x0c, 0x44, 0xd1, 0xab, 0xf4, 0x09, 0xbc, 0x13, 0x42, 0x18,
    0xa2, 0x40, 0x30, 0x01, 0x71, 0xe6, 0xf4, 0x52, 0xde, 0xc2, 0x2a, 0x56, 0xbd, 0xf8, 0x8b, 0x7e,
    0x3d, 0x11, 0x5e, 0x83, 0xfd, 0x13, 0x77, 0x95, 0x59, 0x11, 0xe5, 0x83, 0xc7, 0x28, 0xcd, 0x20,
    0x6f, 0x52, 0xf4, 0x95, 0xb3, 0xbb, 0x4e, 0x04, 0x39, 0x30, 0x13, 0x67, 0x82, 0x43, 0x53, 0x39,
    0xd4, 0x95, 0xb2, 0xba, 0x77, 0x4a, 0x71, 0xe4, 0x7c, 0x62, 0x2a, 0x77, 0xb2, 0x55, 0x03, 0xc5,
    0xec, 0x3a, 0xad, 0xf5, 0x52, 0x9a, 0x92, 0x99, 0x28, 0xb8, 0x42, 0x87, 0x75, 0x4c, 0xee, 0x09,
    0x17, 0xa9, 0xac, 0xdc, 0xa8, 0x06, 0xaa, 0x9e, 0xc9, 0x6e, 0xbf, 0x9f, 0xcd, 0xd8, 0x8c, 0xcd,
    0xd8, 0x8c, 0xcd, 0xf8, 0x73, 0xc6, 0x17,
];

/// A source that hands out at most `n` bytes per `read`.
struct Drip<'a> {
    data: &'a [u8],
    at: usize,
    n: usize,
}

impl<'a> Drip<'a> {
    fn new(data: &'a [u8], n: usize) -> Self {
        Self {
            data,
            at: 0,
            n: n.max(1),
        }
    }
}

impl Read for Drip<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let end = self.data.len().min(self.at + buf.len().min(self.n));
        let src = &self.data[self.at..end];
        buf[..src.len()].copy_from_slice(src);
        self.at = end;
        Ok(src.len())
    }
}

/// Read `r` to the end, taking at most `chunk` bytes per call.
fn read_all_chunked(mut r: impl Read, chunk: usize) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut buf = vec![0u8; chunk.max(1)];
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            return Ok(out);
        }
        out.extend_from_slice(&buf[..n]);
    }
}

/// A deterministic pseudo-random source.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as u32
    }
}

/// `raw` as a raw DEFLATE stream of STORED blocks (at most 65,535 bytes each).
fn stored(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut chunks: Vec<&[u8]> = raw.chunks(65_535).collect();
    if chunks.is_empty() {
        chunks.push(&[]);
    }
    let last = chunks.len() - 1;
    for (i, chunk) in chunks.into_iter().enumerate() {
        out.push(u8::from(i == last));
        let len = chunk.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(chunk);
    }
    out
}

/// One gzip member around a raw DEFLATE `body` whose output is `payload`.
fn gzip_member(header: &[u8], body: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut out = header.to_vec();
    out.extend_from_slice(body);
    out.extend_from_slice(&crc32_of(payload).to_le_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out
}

const PLAIN_HEADER: &[u8] = &[0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 3];

fn gzip(body: &[u8], payload: &[u8]) -> Vec<u8> {
    gzip_member(PLAIN_HEADER, body, payload)
}

fn crc32_of(data: &[u8]) -> u32 {
    aterm_codec::crc32::Crc32::of(data)
}

/// Every vector as `(name, raw deflate, decoded payload)`. Stored payloads
/// straddle the 32 KiB window and the 64 KiB block limit.
fn vectors() -> Vec<(String, Vec<u8>, Vec<u8>)> {
    let mut out = Vec::new();
    for (name, body) in [
        ("hello", HELLO),
        ("backrefs", BACKREFS),
        ("dynamic", DYNAMIC),
    ] {
        let payload = inflate(body, 1 << 20).expect("the one-shot decodes the vector");
        assert!(!payload.is_empty(), "{name} decodes to something");
        out.push((name.to_string(), body.to_vec(), payload));
    }
    let mut rng = Rng(0x1234_5678_9ABC_DEF0);
    for len in [0usize, 1, 32_767, 32_769, 65_535, 65_536, 90_000] {
        let raw: Vec<u8> = (0..len).map(|_| (rng.next() & 0xFF) as u8).collect();
        out.push((format!("stored {len}"), stored(&raw), raw));
    }
    out
}

/// THE RESUMPTION PROPERTY: every read and refill granularity produces the
/// bytes the one-shot decoder produces, raw and gzip-framed.
#[test]
fn every_read_and_refill_granularity_matches_the_one_shot() {
    for (name, body, payload) in vectors() {
        assert_eq!(
            inflate(&body, usize::MAX).expect("one-shot"),
            payload,
            "{name}"
        );
        let gz = gzip(&body, &payload);
        let big = body.len() > 4096;
        for read_chunk in [1usize, 2, 7, 13, 1024, 32 * 1024, 1 << 20] {
            for drip in [1usize, 3, 64, 4096, usize::MAX] {
                if big && read_chunk < 7 && drip < 64 {
                    continue;
                }
                let ours = read_all_chunked(DeflateReader::new(Drip::new(&body, drip)), read_chunk)
                    .unwrap_or_else(|e| panic!("raw {name} r={read_chunk} d={drip}: {e}"));
                assert_eq!(ours, payload, "raw {name} r={read_chunk} d={drip}");
                let ours = read_all_chunked(GzipReader::new(Drip::new(&gz, drip)), read_chunk)
                    .unwrap_or_else(|e| panic!("gz {name} r={read_chunk} d={drip}: {e}"));
                assert_eq!(ours, payload, "gz {name} r={read_chunk} d={drip}");
            }
        }
    }
}

/// A concatenation of gzip members is itself a gzip file — what the `tar-gz`
/// lane depends on, since real producers emit it.
#[test]
fn concatenated_members_are_one_stream() {
    let members: Vec<(Vec<u8>, Vec<u8>)> = vectors()
        .into_iter()
        .map(|(_, body, payload)| (body, payload))
        .collect();
    for count in 1..=members.len() {
        let mut stream = Vec::new();
        let mut expect = Vec::new();
        for (body, payload) in members.iter().take(count) {
            stream.extend_from_slice(&gzip(body, payload));
            expect.extend_from_slice(payload);
        }
        for drip in [1usize, 5, 4096, usize::MAX] {
            let ours = read_all_chunked(GzipReader::new(Drip::new(&stream, drip)), 777)
                .unwrap_or_else(|e| panic!("{count} members, drip {drip}: {e}"));
            assert_eq!(ours, expect, "{count} members, drip {drip}");
        }
    }
}

/// A short or corrupted download is refused before a byte reaches the
/// extractor, and nothing panics. An EMPTY input is a truncated file.
#[test]
fn truncated_and_corrupted_gzip_is_refused() {
    let payload = inflate(DYNAMIC, 1 << 20).expect("one-shot");
    let good = gzip(DYNAMIC, &payload);
    for cut in 0..good.len() {
        assert!(
            read_all_chunked(GzipReader::new(&good[..cut]), 4096).is_err(),
            "truncated to {cut} was accepted"
        );
    }

    let mut rng = Rng(0xDEAD_BEEF_CAFE_F00D);
    for _ in 0..3_000 {
        let mut bytes = good.clone();
        let at = (rng.next() as usize) % bytes.len();
        bytes[at] ^= 1 << (rng.next() % 8);
        if let Ok(out) = read_all_chunked(GzipReader::new(&bytes[..]), 4096) {
            assert_eq!(out, payload, "flip at {at} was accepted with other bytes");
        }
    }
}

/// Malformed raw deflate must never panic.
#[test]
fn corrupted_raw_deflate_never_panics() {
    let mut rng = Rng(0x0BAD_C0DE_1234_5678);
    for good in [DYNAMIC, BACKREFS, HELLO] {
        for _ in 0..2_000 {
            let mut bytes = good.to_vec();
            let at = (rng.next() as usize) % bytes.len();
            bytes[at] ^= 1 << (rng.next() % 8);
            if rng.next().is_multiple_of(4) {
                bytes.truncate((rng.next() as usize) % bytes.len().max(1));
            }
            let _ = read_all_chunked(DeflateReader::new(&bytes[..]), 4096);
        }
    }
}

/// Optional gzip header fields: FNAME, FCOMMENT, FEXTRA and FHCRC in every
/// combination, plus the reserved flag bit, which must be refused.
#[test]
fn optional_header_fields_are_skipped_and_reserved_bits_refused() {
    let payload = b"header field coverage".to_vec();
    let body = stored(&payload);
    for flg in 0u8..=0x3F {
        let mut header: Vec<u8> = vec![0x1f, 0x8b, 8, flg, 0, 0, 0, 0, 0, 3];
        if flg & 0x04 != 0 {
            header.extend_from_slice(&3u16.to_le_bytes());
            header.extend_from_slice(b"abc");
        }
        if flg & 0x08 != 0 {
            header.extend_from_slice(b"name.tar\0");
        }
        if flg & 0x10 != 0 {
            header.extend_from_slice(b"a comment\0");
        }
        if flg & 0x02 != 0 {
            let crc16 = (crc32_of(&header) & 0xFFFF) as u16;
            header.extend_from_slice(&crc16.to_le_bytes());
        }
        let stream = gzip_member(&header, &body, &payload);
        let ours = read_all_chunked(GzipReader::new(Drip::new(&stream, 3)), 64);
        if flg & 0x20 != 0 {
            assert!(ours.is_err(), "FLG {flg:#04x}: a reserved bit was accepted");
        } else {
            assert_eq!(
                ours.unwrap_or_else(|e| panic!("FLG {flg:#04x}: {e}")),
                payload,
                "FLG {flg:#04x}"
            );
        }
    }
}

/// A wrong FHCRC is refused — the one header field that is a checksum.
#[test]
fn a_wrong_header_checksum_is_refused() {
    let payload = b"fhcrc".to_vec();
    let mut header: Vec<u8> = vec![0x1f, 0x8b, 8, 0x02, 0, 0, 0, 0, 0, 3];
    let crc16 = (crc32_of(&header) & 0xFFFF) as u16;
    header.extend_from_slice(&crc16.wrapping_add(1).to_le_bytes());
    let stream = gzip_member(&header, &stored(&payload), &payload);
    assert!(read_all_chunked(GzipReader::new(&stream[..]), 64).is_err());
}

/// The trailer is load-bearing: a member whose CRC or length is wrong is
/// refused even though its deflate stream decodes perfectly.
#[test]
fn a_wrong_trailer_is_refused() {
    let payload = b"trailer".to_vec();
    let base = gzip(&stored(&payload), &payload);
    assert_eq!(
        read_all_chunked(GzipReader::new(&base[..]), 64).expect("intact"),
        payload
    );
    for spoil in 1usize..=8 {
        let mut bytes = base.clone();
        let at = bytes.len() - spoil;
        bytes[at] ^= 0xFF;
        assert!(
            read_all_chunked(GzipReader::new(&bytes[..]), 64).is_err(),
            "trailer byte -{spoil} spoiled but accepted"
        );
    }
}

/// `raw` DEFLATE `body` framed as zlib: header, body, big-endian Adler-32.
fn zlib(body: &[u8], payload: &[u8]) -> Vec<u8> {
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in payload {
        a = (a + u32::from(byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    let mut out = vec![0x78, 0x01];
    out.extend_from_slice(body);
    out.extend_from_slice(&((b << 16) | a).to_be_bytes());
    out
}

/// zlib framing streams exactly as the one-shot PREFIX form decodes: every
/// granularity yields the payload, bytes after the Adler-32 are never part of
/// the stream, and a spoiled header, body end or checksum is refused.
#[test]
fn zlib_streams_like_the_one_shot_prefix_form() {
    for (name, body, payload) in vectors() {
        let z = zlib(&body, &payload);
        let (one_shot, used) = zlib_decompress_prefix(&z, usize::MAX).expect("one-shot");
        assert_eq!(
            (one_shot.as_slice(), used),
            (payload.as_slice(), z.len()),
            "{name}"
        );
        let mut padded = z.clone();
        padded.extend_from_slice(&[0xAB; 70]);
        for read_chunk in [1usize, 13, 4096, 1 << 20] {
            for drip in [1usize, 64, usize::MAX] {
                if body.len() > 4096 && read_chunk < 13 && drip < 64 {
                    continue;
                }
                for stream in [&z, &padded] {
                    let ours =
                        read_all_chunked(ZlibReader::new(Drip::new(stream, drip)), read_chunk)
                            .unwrap_or_else(|e| panic!("{name} r={read_chunk} d={drip}: {e}"));
                    assert_eq!(ours, payload, "{name} r={read_chunk} d={drip}");
                }
            }
        }
        for cut in [0, 1, 2, z.len() - 4, z.len() - 1] {
            assert!(
                read_all_chunked(ZlibReader::new(&z[..cut]), 4096).is_err(),
                "{name} truncated to {cut} was accepted"
            );
        }
        for spoil in [0usize, 1, z.len() - 1] {
            let mut bytes = z.clone();
            bytes[spoil] ^= 0x01;
            assert!(
                read_all_chunked(ZlibReader::new(&bytes[..]), 4096).is_err(),
                "{name}: byte {spoil} spoiled but accepted"
            );
        }
    }
}
