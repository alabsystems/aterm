// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Round-trip through aterm's OWN inflate — the decoder this crate's PNG
/// reader uses, so a stream that survives this is a stream aterm can read.
fn roundtrip(data: &[u8]) {
    let z = zlib_compress(data);
    let back = aterm_codec::inflate::zlib_decompress(&z, data.len() + 64)
        .unwrap_or_else(|e| panic!("inflate failed on {} bytes: {e:?}", data.len()));
    assert_eq!(back, data, "round-trip differed at {} bytes", data.len());
}

#[test]
fn roundtrips_edge_shapes() {
    roundtrip(b"");
    roundtrip(b"a");
    roundtrip(b"ab");
    roundtrip(b"abc");
    roundtrip(b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    roundtrip(&[0u8; 1024]);
    roundtrip(&[0xFFu8; 300_000]);
    roundtrip(b"the quick brown fox jumps over the lazy dog");
    let all: Vec<u8> = (0..=255u8).collect();
    roundtrip(&all);
    let repeated: Vec<u8> = all.iter().cycle().take(70_000).copied().collect();
    roundtrip(&repeated);
}

/// The streaming reader PNG decode reads with hands back what `z` holds, read a row's worth
/// at a time and all at once.
fn streams_back(z: &[u8], data: &[u8]) {
    use std::io::Read as _;
    for chunk in [4096, 1 << 20] {
        let mut reader = aterm_codec::inflate::stream::ZlibReader::new(z);
        let mut buf = vec![0u8; chunk];
        let mut back = Vec::with_capacity(data.len());
        loop {
            let n = reader
                .read(&mut buf)
                .unwrap_or_else(|e| panic!("the stream refused at {} bytes: {e}", back.len()));
            if n == 0 {
                break;
            }
            back.extend_from_slice(&buf[..n]);
        }
        assert!(back == data, "the stream differed, read {chunk} at a time");
    }
}

/// A maximal match (258 bytes) and a maximal distance (32768) must both be
/// expressible — the two ends of the code tables — through the one-shot inflate and through
/// the streaming reader, which keeps only the window behind its write head.
#[test]
fn roundtrips_extreme_match_geometry() {
    let mut max_len = vec![7u8; 258];
    max_len.extend(std::iter::repeat_n(0u8, WINDOW - 258));
    max_len.extend(std::iter::repeat_n(7u8, 258));
    roundtrip(&max_len);
    streams_back(&zlib_compress(&max_len), &max_len);

    // Place `[9, 8, 7]` only at index 0 and index `WINDOW` (32768) so the greedy
    // matcher has no nearer `[9, 8, 7]` candidate and emits distance 32768 (code 29,
    // all 13 extra bits set).
    let mut max_dist = vec![9u8, 8, 7];
    max_dist.extend(std::iter::repeat_n(0u8, WINDOW - 3));
    max_dist.extend_from_slice(&[9, 8, 7]);
    roundtrip(&max_dist);
    streams_back(&zlib_compress(&max_dist), &max_dist);

    // Noise repeating at exactly the window, past where the reader first compacts: nothing
    // nearer repeats, so every match after the first period is at distance 32768, and the
    // first symbol after each compaction reaches back to the oldest byte the reader kept.
    let mut state = 0x1234_5678u32;
    let period: Vec<u8> = (0..WINDOW)
        .map(|_| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 24) as u8
        })
        .collect();
    let repeated = period.repeat(8);
    let z = zlib_compress(&repeated);
    assert!(
        z.len() < 2 * WINDOW,
        "CONTROL: {} bytes: the repeats are matches at the window's own distance",
        z.len()
    );
    roundtrip(&repeated);
    streams_back(&z, &repeated);
}

/// Pseudo-random bytes are incompressible; the stream must still be valid
/// (this is where a fixed block is at its worst and the bit packing is most
/// likely to be caught out).
#[test]
fn roundtrips_incompressible_noise() {
    let mut state = 0x1234_5678u32;
    let noise: Vec<u8> = (0..50_000)
        .map(|_| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 24) as u8
        })
        .collect();
    roundtrip(&noise);
}

/// Structured, image-like input across two shapes that stress different
/// halves of the matcher:
///
/// * a raw RGBA gradient, whose per-pixel bytes all change;
/// * a PNG-filtered residual stream with long runs of small residuals.
#[test]
fn roundtrips_image_like_input() {
    let mut raw = Vec::new();
    for y in 0..200u32 {
        raw.push(0); // filter byte
        for x in 0..200u32 {
            raw.extend_from_slice(&[(x % 256) as u8, (y % 256) as u8, 128, 255]);
        }
    }
    roundtrip(&raw);

    let mut filtered = Vec::new();
    for _ in 0..200 {
        filtered.push(1u8); // Sub filter
        filtered.extend(std::iter::repeat_n(0u8, 400));
        filtered.extend((0..400u32).map(|i| ((i * 7) % 3) as u8));
    }
    roundtrip(&filtered);
}

#[test]
fn zlib_header_is_well_formed() {
    let z = zlib_compress(b"hello");
    assert_eq!(z[0] & 0x0F, 8, "CM must be deflate");
    assert_eq!(z[0] >> 4, 7, "CINFO must be the 32 KiB window");
    assert_eq!(
        (u16::from(z[0]) << 8 | u16::from(z[1])) % 31,
        0,
        "FCHECK must make the header pair a multiple of 31"
    );
    assert_eq!(z[1] & 0x20, 0, "no preset dictionary");
    let n = z.len();
    assert_eq!(
        u32::from_be_bytes([z[n - 4], z[n - 3], z[n - 2], z[n - 1]]),
        super::super::checksum::adler32(b"hello"),
    );
}

/// Reference whole-buffer compressor with one `prev` link per input position.
fn whole_buffer_zlib(data: &[u8]) -> Vec<u8> {
    let mut w = BitWriter::with_capacity(0);
    w.bits(1, 1);
    w.bits(1, 2);
    let mut head = vec![usize::MAX; HASH_SIZE];
    let mut prev = vec![usize::MAX; data.len()];
    let mut i = 0usize;
    while i < data.len() {
        let mut best_len = 0usize;
        let mut best_dist = 0usize;
        if i + MIN_MATCH <= data.len() {
            let h = hash3(data, i);
            let mut candidate = head[h];
            let mut chain = 0usize;
            let limit = (data.len() - i).min(MAX_MATCH);
            while candidate != usize::MAX
                && chain < MAX_CHAIN
                && best_len < GOOD_ENOUGH
                && best_len < limit
            {
                chain += 1;
                let dist = i - candidate;
                if dist > WINDOW {
                    break;
                }
                if best_len >= MIN_MATCH
                    && i + best_len < data.len()
                    && data[candidate + best_len] != data[i + best_len]
                {
                    candidate = prev[candidate];
                    continue;
                }
                let mut len = 0usize;
                while len < limit && data[candidate + len] == data[i + len] {
                    len += 1;
                }
                if len > best_len {
                    best_len = len;
                    best_dist = dist;
                }
                candidate = prev[candidate];
            }
        }
        if best_len >= MIN_MATCH {
            let (code, extra, extra_bits) = length_code(best_len as u16);
            let (lit_code, lit_len) = fixed_litlen(code);
            w.huffman(lit_code, lit_len);
            if extra_bits > 0 {
                w.bits(extra, extra_bits);
            }
            let (dcode, dextra, dextra_bits) = distance_code(best_dist as u16);
            w.huffman(dcode, 5);
            if dextra_bits > 0 {
                w.bits(dextra, dextra_bits);
            }
            for (k, slot) in prev.iter_mut().enumerate().skip(i).take(best_len) {
                if k + MIN_MATCH <= data.len() {
                    let h = hash3(data, k);
                    *slot = head[h];
                    head[h] = k;
                }
            }
            i += best_len;
        } else {
            let (code, len) = fixed_litlen(u16::from(data[i]));
            w.huffman(code, len);
            if i + MIN_MATCH <= data.len() {
                let h = hash3(data, i);
                prev[i] = head[h];
                head[h] = i;
            }
            i += 1;
        }
    }
    let (code, len) = fixed_litlen(256);
    w.huffman(code, len);
    let mut out = vec![0x78, 0x01];
    out.extend_from_slice(&w.finish());
    out.extend_from_slice(&super::super::checksum::adler32(data).to_be_bytes());
    out
}

/// Seeded inputs that reach every part of the window logic: noise,
/// filtered-scanline shapes, runs far longer than the window, and repeats
/// at distances on both sides of it.
fn window_cases() -> Vec<(&'static str, Vec<u8>)> {
    let mut state = 0x2545_F491u32;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        state
    };
    let noise: Vec<u8> = (0..3 * WINDOW + 777)
        .map(|_| (next() >> 24) as u8)
        .collect();
    let mut scanlines = Vec::new();
    for y in 0..400u32 {
        scanlines.push((y % 5) as u8);
        scanlines.extend((0..1024u32).map(|x| ((x * 3 + y) % 7) as u8));
    }
    let run = vec![0xA5u8; 5 * WINDOW + 13];
    let block: Vec<u8> = (0..300).map(|_| (next() >> 24) as u8).collect();
    let mut straddle = block.clone();
    straddle.extend((0..WINDOW - block.len()).map(|_| (next() >> 24) as u8));
    straddle.extend_from_slice(&block);
    straddle.extend((0..WINDOW - block.len() + 1).map(|_| (next() >> 24) as u8));
    straddle.extend_from_slice(&block);
    let mut mixed = Vec::new();
    for i in 0..40u32 {
        let len = (next() % 9000) as usize;
        if i % 3 == 0 {
            mixed.extend(std::iter::repeat_n((next() >> 24) as u8, len));
        } else {
            mixed.extend((0..len).map(|_| (next() >> 28) as u8));
        }
    }
    vec![
        ("empty", Vec::new()),
        ("short", b"abcabcabcabd".to_vec()),
        ("noise", noise),
        ("scanlines", scanlines),
        ("run", run),
        ("straddle", straddle),
        ("mixed", mixed),
    ]
}

#[test]
fn streaming_output_is_byte_identical_to_the_whole_buffer_compressor() {
    let mut state = 0x9E37_79B9u32;
    for (label, data) in window_cases() {
        let oracle = whole_buffer_zlib(&data);
        assert_eq!(zlib_compress(&data), oracle, "{label}: one write");

        let mut rows = ZlibStream::new();
        for row in data.chunks(1025) {
            rows.write(row);
        }
        assert_eq!(rows.finish(), oracle, "{label}: scanline writes");

        let mut random = ZlibStream::new();
        let mut rest = &data[..];
        while !rest.is_empty() {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let take = (state as usize % (2 * WINDOW)).min(rest.len());
            random.write(&rest[..take]);
            rest = &rest[take..];
        }
        assert_eq!(random.finish(), oracle, "{label}: random splits");

        if data.len() <= 3 * WINDOW + 1000 {
            let mut bytes = ZlibStream::new();
            for byte in &data {
                bytes.write(std::slice::from_ref(byte));
            }
            assert_eq!(bytes.finish(), oracle, "{label}: byte writes");
        }
    }
}

/// The compressor's memory is bounded by [`STATE_BYTES`], not by its input:
/// a 1 MiB write and a scanline-at-a-time stream leave the window at the
/// capacity it was born with, and the chains at one window.
#[test]
fn the_window_never_outgrows_its_capacity() {
    let data: Vec<u8> = (0..(1usize << 20)).map(|i| (i * 31 % 251) as u8).collect();
    for split in [data.len(), 4097] {
        let mut stream = ZlibStream::new();
        for piece in data.chunks(split) {
            stream.write(piece);
            assert_eq!(stream.window.capacity(), WINDOW_CAPACITY, "split {split}");
            assert!(stream.window.len() <= WINDOW_CAPACITY, "split {split}");
        }
        assert_eq!((stream.head.len(), stream.prev.len()), (HASH_SIZE, WINDOW));
        assert_eq!(stream.finish(), whole_buffer_zlib(&data), "split {split}");
    }
}

#[test]
fn deflate_code_tables_cover_every_length_and_distance() {
    for sym in 0..=287u16 {
        let (code, bits) = fixed_litlen(sym);
        let expected = match sym {
            0..=143 => (0x30 + sym, 8),
            144..=255 => (0x190 + (sym - 144), 9),
            256..=279 => (sym - 256, 7),
            _ => (0xC0 + (sym - 280), 8),
        };
        assert_eq!((code, bits), expected, "sym {sym}");
    }
    for l in 3..=258u16 {
        let (code, extra_val, extra_bits) = length_code(l);
        assert!((257..=285).contains(&code), "len {l}");
        let idx = usize::from(code - 257);
        assert_eq!(extra_bits, LENGTH_EXTRA[idx], "len {l}");
        assert!(extra_val < (1u32 << extra_bits), "len {l}");
        assert_eq!(
            u32::from(LENGTH_BASE[idx]) + extra_val,
            u32::from(l),
            "len {l}"
        );
    }
    for d in 1..=32768u16 {
        let (code, extra_val, extra_bits) = distance_code(d);
        assert!(code <= 29, "dist {d}");
        let idx = usize::from(code);
        assert_eq!(extra_bits, DIST_EXTRA[idx], "dist {d}");
        assert!(extra_val < (1u32 << extra_bits), "dist {d}");
        assert_eq!(
            u32::from(DIST_BASE[idx]) + extra_val,
            u32::from(d),
            "dist {d}"
        );
    }
}
