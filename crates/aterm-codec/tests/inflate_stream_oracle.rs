// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Differential oracle: `aterm_codec::inflate::stream` against the retired
//! `flate2` package (pure-Rust `miniz_oxide` backend) it replaces.
//!
//! atpkg's `tar-gz` and `zip` install lanes hand a decompressed stream straight
//! to a walker that writes files to disk, so the two properties that matter are
//! "the same bytes come out" and "the same malformed inputs are refused". Both
//! are asserted here against `flate2` itself.
//!
//! The interesting risk in a streaming decoder is not the DEFLATE algorithm —
//! that engine is shared with the one-shot `inflate` and has its own tests — it
//! is the RESUMPTION: a unit of work interrupted by the input running out has to
//! restart from exactly where it began. So the corpus is run at read
//! granularities down to a single byte AND with a source that yields a single
//! byte per `read`, which forces a checkpoint-restore on very nearly every
//! symbol, block header and table read in the stream.

use std::io::{Read, Write};

use aterm_codec::inflate::stream::{DeflateReader, GzipReader};

// ── helpers ────────────────────────────────────────────────────────────────

/// A source that hands out at most `n` bytes per `read`, so the decoder is
/// starved on nearly every unit of work.
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
        let want = buf.len().min(self.n);
        let end = self.data.len().min(self.at + want);
        let src = &self.data[self.at..end];
        buf[..src.len()].copy_from_slice(src);
        self.at = end;
        Ok(src.len())
    }
}

fn gzip_with(raw: &[u8], level: u32) -> Vec<u8> {
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(level));
    enc.write_all(raw).unwrap();
    enc.finish().unwrap()
}

fn deflate_with(raw: &[u8], level: u32) -> Vec<u8> {
    let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(level));
    enc.write_all(raw).unwrap();
    enc.finish().unwrap()
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

fn oracle_gunzip(bytes: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    flate2::read::MultiGzDecoder::new(bytes).read_to_end(&mut out)?;
    Ok(out)
}

/// A deterministic, dependency-free pseudo-random source for the corpus.
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

/// Payloads chosen for what they make DEFLATE emit: incompressible data forces
/// STORED blocks, long runs force overlapping (RLE) back-references, and text
/// with a large vocabulary forces dynamic Huffman tables. Sizes straddle the
/// 32 KiB window and the 32 KiB pump target in both directions.
fn corpus() -> Vec<(String, Vec<u8>)> {
    let mut rng = Rng(0x5DEE_CE66_D1E5_C0DE);
    let mut out: Vec<(String, Vec<u8>)> = Vec::new();
    out.push(("empty".into(), Vec::new()));
    out.push(("one byte".into(), vec![0x42]));
    for len in [
        1usize, 2, 3, 255, 4096, 32_767, 32_768, 32_769, 65_536, 200_000,
    ] {
        out.push((format!("zeros {len}"), vec![0u8; len]));
        out.push((
            format!("incompressible {len}"),
            (0..len).map(|_| (rng.next() & 0xFF) as u8).collect(),
        ));
        out.push((
            format!("runs {len}"),
            (0..len).map(|i| ((i / 97) % 251) as u8).collect(),
        ));
        let mut text = Vec::with_capacity(len);
        while text.len() < len {
            let word = (rng.next() % 512) as u16;
            text.extend_from_slice(format!("w{word} ").as_bytes());
        }
        text.truncate(len);
        out.push((format!("text {len}"), text));
    }
    out
}

// ── the properties ─────────────────────────────────────────────────────────

/// Gzip: same bytes as `flate2::read::MultiGzDecoder`, at every level, for the
/// whole corpus.
#[test]
fn gzip_matches_the_oracle_over_the_corpus() {
    for (name, raw) in corpus() {
        for level in [0u32, 1, 6, 9] {
            let gz = gzip_with(&raw, level);
            let ours = read_all_chunked(GzipReader::new(&gz[..]), 8192)
                .unwrap_or_else(|e| panic!("{name} @{level}: {e}"));
            let theirs = oracle_gunzip(&gz).unwrap_or_else(|e| panic!("{name} @{level}: {e}"));
            assert_eq!(ours, raw, "{name} @{level}: gzip round-trip lost bytes");
            assert_eq!(ours, theirs, "{name} @{level}: disagreed with the oracle");
        }
    }
}

/// Raw deflate: same bytes as `flate2::read::DeflateDecoder`, and the same bytes
/// as this crate's own one-shot `inflate` — one engine, two drivers.
#[test]
fn raw_deflate_matches_the_oracle_and_the_one_shot() {
    for (name, raw) in corpus() {
        for level in [0u32, 1, 6, 9] {
            let def = deflate_with(&raw, level);
            let ours = read_all_chunked(DeflateReader::new(&def[..]), 8192)
                .unwrap_or_else(|e| panic!("{name} @{level}: {e}"));
            let mut theirs = Vec::new();
            flate2::read::DeflateDecoder::new(&def[..])
                .read_to_end(&mut theirs)
                .unwrap_or_else(|e| panic!("{name} @{level}: {e}"));
            let one_shot = aterm_codec::inflate::inflate(&def, usize::MAX).unwrap();
            assert_eq!(ours, raw, "{name} @{level}: deflate round-trip lost bytes");
            assert_eq!(ours, theirs, "{name} @{level}: disagreed with the oracle");
            assert_eq!(ours, one_shot, "{name} @{level}: streaming != one-shot");
        }
    }
}

/// THE RESUMPTION PROPERTY. Every combination of read granularity and source
/// granularity must produce the identical byte string — including one byte at a
/// time on both sides, which interrupts very nearly every unit of work in the
/// stream and forces the checkpoint to be restored and replayed.
#[test]
fn every_read_and_refill_granularity_produces_the_same_bytes() {
    let mut rng = Rng(0x1234_5678_9ABC_DEF0);
    // Big enough to span many blocks and to cross the window and pump target,
    // small enough to run at one-byte granularity in reasonable time.
    let raw: Vec<u8> = (0..90_000)
        .map(|i| {
            if i % 3 == 0 {
                (rng.next() & 0xFF) as u8
            } else {
                b"the quick brown fox jumps over the lazy dog "[(i / 3) % 43]
            }
        })
        .collect();
    for level in [0u32, 1, 6, 9] {
        let gz = gzip_with(&raw, level);
        let def = deflate_with(&raw, level);
        for read_chunk in [1usize, 2, 7, 13, 1024, 32 * 1024, 1 << 20] {
            for drip in [1usize, 3, 64, 4096, usize::MAX] {
                let ours = read_all_chunked(GzipReader::new(Drip::new(&gz, drip)), read_chunk)
                    .unwrap_or_else(|e| panic!("gz @{level} r={read_chunk} d={drip}: {e}"));
                assert_eq!(ours, raw, "gz @{level} r={read_chunk} d={drip}");
                let ours = read_all_chunked(DeflateReader::new(Drip::new(&def, drip)), read_chunk)
                    .unwrap_or_else(|e| panic!("raw @{level} r={read_chunk} d={drip}: {e}"));
                assert_eq!(ours, raw, "raw @{level} r={read_chunk} d={drip}");
            }
        }
    }
}

/// A concatenation of gzip members is itself a gzip file — the `MultiGzDecoder`
/// behaviour the `tar-gz` lane depends on, since real producers emit it.
#[test]
fn concatenated_members_match_the_oracle() {
    let parts: Vec<Vec<u8>> = vec![
        b"first member\n".to_vec(),
        Vec::new(),
        vec![7u8; 70_000],
        b"last".to_vec(),
    ];
    for count in 1..=parts.len() {
        let mut stream = Vec::new();
        let mut expect = Vec::new();
        for part in parts.iter().take(count) {
            stream.extend_from_slice(&gzip_with(part, 6));
            expect.extend_from_slice(part);
        }
        for drip in [1usize, 5, 4096, usize::MAX] {
            let ours = read_all_chunked(GzipReader::new(Drip::new(&stream, drip)), 777)
                .unwrap_or_else(|e| panic!("{count} members, drip {drip}: {e}"));
            assert_eq!(ours, expect, "{count} members, drip {drip}");
        }
        assert_eq!(oracle_gunzip(&stream).unwrap(), expect);
    }
}

/// The repository's own bytes as a real corpus, gzipped and read back byte for
/// byte — the shape the `aterm-toml` and `aterm-png` retirements were held to.
#[test]
fn repository_corpus_round_trips_byte_identically() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .to_path_buf();
    let mut stack = vec![root];
    let mut files = 0usize;
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                if path.file_name().is_some_and(|n| n == "target") {
                    continue;
                }
                stack.push(path);
            } else if meta.is_file() && meta.len() <= 512 * 1024 {
                let Ok(raw) = std::fs::read(&path) else {
                    continue;
                };
                let gz = gzip_with(&raw, 6);
                let ours = read_all_chunked(GzipReader::new(&gz[..]), 4096)
                    .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
                assert_eq!(ours, raw, "{}", path.display());
                assert_eq!(ours, oracle_gunzip(&gz).unwrap(), "{}", path.display());
                files += 1;
            }
        }
    }
    assert!(files > 500, "corpus too small: {files} files");
}

/// Malformed gzip: the verdict must agree with `flate2`, and NOTHING may panic.
///
/// Truncation and bit-flips are the two ways a signed archive arrives broken —
/// a short download and a corrupted one — and both must be refused before a
/// byte reaches the extractor.
#[test]
fn corrupted_gzip_is_refused_exactly_as_the_oracle_refuses_it() {
    let raw: Vec<u8> = (0..40_000u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8)
        .collect();
    let good = gzip_with(&raw, 6);

    // Every truncation length, sampled.
    for cut in (0..good.len()).step_by((good.len() / 200).max(1)) {
        let bytes = &good[..cut];
        let ours = read_all_chunked(GzipReader::new(bytes), 4096);
        let theirs = oracle_gunzip(bytes);
        assert_eq!(
            ours.is_ok(),
            theirs.is_ok(),
            "truncated to {cut}: verdicts differ (ours={:?}, oracle={:?})",
            ours.as_ref().map(Vec::len).map_err(|e| e.to_string()),
            theirs.as_ref().map(Vec::len).map_err(|e| e.to_string()),
        );
        if let (Ok(a), Ok(b)) = (&ours, &theirs) {
            assert_eq!(a, b, "truncated to {cut}");
        }
    }

    // Single-byte mutations across the whole stream: header, deflate body and
    // trailer alike.
    let mut rng = Rng(0xDEAD_BEEF_CAFE_F00D);
    for _ in 0..3_000 {
        let mut bytes = good.clone();
        let at = (rng.next() as usize) % bytes.len();
        bytes[at] ^= 1 << (rng.next() % 8);
        let ours = read_all_chunked(GzipReader::new(&bytes[..]), 4096);
        let theirs = oracle_gunzip(&bytes);
        assert_eq!(
            ours.is_ok(),
            theirs.is_ok(),
            "flip at {at}: verdicts differ (ours={:?}, oracle={:?})",
            ours.as_ref().map(Vec::len).map_err(|e| e.to_string()),
            theirs.as_ref().map(Vec::len).map_err(|e| e.to_string()),
        );
        if let (Ok(a), Ok(b)) = (&ours, &theirs) {
            assert_eq!(a, b, "flip at {at}");
        }
    }
}

/// Malformed raw deflate must never panic. Verdicts are NOT asserted equal
/// here: `flate2`'s reader reports a stream that simply runs out as a clean
/// end-of-file, while this one calls truncation an error. That difference is
/// deliberate — see the note in the zip lane — so the assertion is the one that
/// still binds: when BOTH accept, they must agree on the bytes.
#[test]
fn corrupted_raw_deflate_never_panics_and_agrees_when_both_accept() {
    let raw: Vec<u8> = (0..20_000u32).map(|i| (i % 253) as u8).collect();
    let good = deflate_with(&raw, 6);
    let mut rng = Rng(0x0BAD_C0DE_1234_5678);
    for _ in 0..4_000 {
        let mut bytes = good.clone();
        let at = (rng.next() as usize) % bytes.len();
        bytes[at] ^= 1 << (rng.next() % 8);
        if rng.next().is_multiple_of(4) {
            bytes.truncate((rng.next() as usize) % bytes.len().max(1));
        }
        let ours = read_all_chunked(DeflateReader::new(&bytes[..]), 4096);
        let mut theirs = Vec::new();
        let theirs = flate2::read::DeflateDecoder::new(&bytes[..])
            .read_to_end(&mut theirs)
            .map(|_| theirs);
        if let (Ok(a), Ok(b)) = (&ours, &theirs) {
            assert_eq!(a, b, "flip at {at}: both accepted, bytes differ");
        }
    }
}

/// Optional gzip header fields: FNAME, FCOMMENT, FEXTRA and FHCRC in every
/// combination, plus a reserved flag bit. Built by hand because `flate2`'s
/// encoder never emits most of them, and `gzip(1)` emits FNAME — so a lane that
/// mishandled one would not be caught by round-tripping alone.
#[test]
fn optional_header_fields_match_the_oracle() {
    let payload = b"header field coverage".to_vec();
    let body = {
        // A bare deflate stream, which is what sits between a gzip header and
        // its trailer.
        deflate_with(&payload, 6)
    };
    let trailer = {
        let mut t = Vec::new();
        t.extend_from_slice(&crc32_of(&payload).to_le_bytes());
        t.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        t
    };
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
        let mut stream = header;
        stream.extend_from_slice(&body);
        stream.extend_from_slice(&trailer);

        let ours = read_all_chunked(GzipReader::new(&stream[..]), 64);
        let theirs = oracle_gunzip(&stream);
        assert_eq!(
            ours.is_ok(),
            theirs.is_ok(),
            "FLG {flg:#04x}: verdicts differ (ours={:?}, oracle={:?})",
            ours.as_ref().map(Vec::len).map_err(|e| e.to_string()),
            theirs.as_ref().map(Vec::len).map_err(|e| e.to_string()),
        );
        if let (Ok(a), Ok(b)) = (&ours, &theirs) {
            assert_eq!(a, b, "FLG {flg:#04x}");
            assert_eq!(a, &payload, "FLG {flg:#04x}");
        }
    }
}

/// A wrong FHCRC must be refused — the one header field that is a checksum, and
/// so the one whose omission would go unnoticed by a round-trip test.
#[test]
fn a_wrong_header_checksum_is_refused_like_the_oracle() {
    let payload = b"fhcrc".to_vec();
    let mut header: Vec<u8> = vec![0x1f, 0x8b, 8, 0x02, 0, 0, 0, 0, 0, 3];
    let crc16 = (crc32_of(&header) & 0xFFFF) as u16;
    header.extend_from_slice(&crc16.wrapping_add(1).to_le_bytes());
    let mut stream = header;
    stream.extend_from_slice(&deflate_with(&payload, 6));
    stream.extend_from_slice(&crc32_of(&payload).to_le_bytes());
    stream.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    let ours = read_all_chunked(GzipReader::new(&stream[..]), 64);
    let theirs = oracle_gunzip(&stream);
    assert_eq!(
        ours.is_ok(),
        theirs.is_ok(),
        "bad FHCRC: verdicts differ (ours={:?}, oracle={:?})",
        ours.as_ref().map(Vec::len).map_err(|e| e.to_string()),
        theirs.as_ref().map(Vec::len).map_err(|e| e.to_string()),
    );
}

/// The trailer is load-bearing: a member whose CRC or length is wrong must be
/// refused even though its deflate stream decodes perfectly.
#[test]
fn a_wrong_trailer_is_refused_like_the_oracle() {
    let payload = b"trailer".to_vec();
    let base = gzip_with(&payload, 6);
    for spoil in [1usize, 2, 3, 4, 5, 6, 7, 8] {
        let mut bytes = base.clone();
        let at = bytes.len() - spoil;
        bytes[at] ^= 0xFF;
        let ours = read_all_chunked(GzipReader::new(&bytes[..]), 64);
        let theirs = oracle_gunzip(&bytes);
        assert_eq!(
            ours.is_ok(),
            theirs.is_ok(),
            "trailer byte -{spoil}: verdicts differ (ours={:?}, oracle={:?})",
            ours.as_ref().map(Vec::len).map_err(|e| e.to_string()),
            theirs.as_ref().map(Vec::len).map_err(|e| e.to_string()),
        );
    }
}

fn crc32_of(data: &[u8]) -> u32 {
    aterm_codec::crc32::Crc32::of(data)
}
