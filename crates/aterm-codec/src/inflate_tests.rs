// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

const EMPTY: &[u8] = &[0x78, 0x9c, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01];
const HELLO: &[u8] = &[
    0x78, 0x9c, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0xd7, 0x51, 0x28, 0xcf, 0x2f, 0xca, 0x49, 0x01, 0x00,
    0x1d, 0x54, 0x04, 0x89,
];
const STORED: &[u8] = &[
    0x78, 0x01, 0x01, 0x0a, 0x00, 0xf5, 0xff, 0x41, 0x42, 0x43, 0x44, 0x45, 0x41, 0x42, 0x43, 0x44,
    0x45, 0x0e, 0x5b, 0x02, 0x9f,
];
const BACKREFS: &[u8] = &[
    0x78, 0x9c, 0x4b, 0x4c, 0x4a, 0xa4, 0x2a, 0x04, 0x00, 0xd2, 0x74, 0x1e, 0x79,
];
const DYNAMIC: &[u8] = &[
    0x78, 0xda, 0xed, 0xce, 0x41, 0x12, 0x02, 0x21, 0x0c, 0x44, 0xd1, 0xab, 0xf4, 0x09, 0xbc, 0x13,
    0x42, 0x18, 0xa2, 0x40, 0x30, 0x01, 0x71, 0xe6, 0xf4, 0x52, 0xde, 0xc2, 0x2a, 0x56, 0xbd, 0xf8,
    0x8b, 0x7e, 0x3d, 0x11, 0x5e, 0x83, 0xfd, 0x13, 0x77, 0x95, 0x59, 0x11, 0xe5, 0x83, 0xc7, 0x28,
    0xcd, 0x20, 0x6f, 0x52, 0xf4, 0x95, 0xb3, 0xbb, 0x4e, 0x04, 0x39, 0x30, 0x13, 0x67, 0x82, 0x43,
    0x53, 0x39, 0xd4, 0x95, 0xb2, 0xba, 0x77, 0x4a, 0x71, 0xe4, 0x7c, 0x62, 0x2a, 0x77, 0xb2, 0x55,
    0x03, 0xc5, 0xec, 0x3a, 0xad, 0xf5, 0x52, 0x9a, 0x92, 0x99, 0x28, 0xb8, 0x42, 0x87, 0x75, 0x4c,
    0xee, 0x09, 0x17, 0xa9, 0xac, 0xdc, 0xa8, 0x06, 0xaa, 0x9e, 0xc9, 0x6e, 0xbf, 0x9f, 0xcd, 0xd8,
    0x8c, 0xcd, 0xd8, 0x8c, 0xcd, 0xf8, 0x73, 0xc6, 0x17, 0xc1, 0xd0, 0x5c, 0x27,
];

#[test]
fn prefix_form_tolerates_bytes_after_the_stream() {
    for vector in [EMPTY, HELLO, STORED, BACKREFS, DYNAMIC] {
        let (plain, used) = zlib_decompress_prefix(vector, 1 << 20).unwrap();
        assert_eq!(used, vector.len(), "a bare stream consumes all of itself");
        assert_eq!(plain, zlib_decompress(vector, 1 << 20).unwrap());
        for pad in [1usize, 4, 64, 65, 1024] {
            let mut padded = vector.to_vec();
            padded.extend(std::iter::repeat_n(0xABu8, pad));
            let (got, used) = zlib_decompress_prefix(&padded, 1 << 20)
                .unwrap_or_else(|e| panic!("{pad} bytes of padding: {e:?}"));
            assert_eq!(got, plain, "{pad} bytes of padding changed the output");
            assert_eq!(used, vector.len(), "{pad} bytes of padding moved the end");
            assert!(
                zlib_decompress(&padded, 1 << 20).is_err(),
                "the whole-buffer form is expected to read the padding as the checksum",
            );
        }
    }
}

#[test]
fn whole_buffer_form_reads_the_checksum_where_the_stream_ends() {
    let stray_before_trailer = [
        0x78, 0x01, 0x03, 0x40, 0x00, 0xff, 0xff, 0x00, 0x00, 0x00, 0x01,
    ];
    assert_eq!(
        zlib_decompress(&stray_before_trailer, 1 << 20),
        Err(InflateError::BadChecksum),
    );
    for vector in [EMPTY, HELLO, STORED, BACKREFS, DYNAMIC] {
        let (body, trailer) = vector.split_at(vector.len() - 4);
        for stray in [1usize, 3, 64] {
            let mut spliced = body.to_vec();
            spliced.extend(std::iter::repeat_n(0u8, stray));
            spliced.extend_from_slice(trailer);
            assert_eq!(
                zlib_decompress(&spliced, 1 << 20),
                Err(InflateError::BadChecksum),
                "{stray} stray bytes between a {}-byte stream and its checksum",
                vector.len(),
            );
        }
    }
}

#[test]
fn prefix_form_still_checks_the_adler_and_the_length() {
    let mut bad = HELLO.to_vec();
    let last = bad.len() - 1;
    bad[last] ^= 0xFF;
    assert_eq!(
        zlib_decompress_prefix(&bad, 1 << 20),
        Err(InflateError::BadChecksum),
    );
    let short = &HELLO[..HELLO.len() - 1];
    assert_eq!(
        zlib_decompress_prefix(short, 1 << 20),
        Err(InflateError::Truncated),
    );
}

#[test]
fn reference_vectors_decode() {
    let dynamic = "the quick brown fox jumps over the lazy dog while a programmer \
                   carefully writes a deflate decompressor in rust with zero dependencies. "
        .repeat(12);
    for (name, vector, want) in [
        ("empty", EMPTY, Vec::new()),
        ("fixed literals", HELLO, b"hello, world".to_vec()),
        ("stored", STORED, b"ABCDEABCDE".to_vec()),
        ("fixed backrefs", BACKREFS, "ab".repeat(40).into_bytes()),
        ("dynamic", DYNAMIC, dynamic.into_bytes()),
    ] {
        assert_eq!(
            zlib_decompress(vector, 1 << 20).as_deref(),
            Ok(&want[..]),
            "{name}"
        );
    }
    let raw = &HELLO[2..HELLO.len() - 4];
    assert_eq!(inflate(raw, 1 << 20).unwrap(), b"hello, world");
}

#[test]
fn output_cap_rejects_bomb() {
    assert_eq!(
        zlib_decompress(DYNAMIC, 10),
        Err(InflateError::OutputTooLarge)
    );
}

#[test]
fn bad_zlib_header_rejected() {
    assert_eq!(
        zlib_decompress(&[0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01], 1 << 20),
        Err(InflateError::BadZlibHeader)
    );
}

#[test]
fn incomplete_dynamic_huffman_rejected() {
    const INCOMPLETE: &[u8] = &[
        0x78, 0x01, 0x05, 0xc0, 0x01, 0x09, 0x00, 0x00, 0x00, 0x80, 0xa0, 0x6d, 0xfd, 0x3f, 0x25,
        0x01, 0x00, 0x42, 0x00, 0x42,
    ];
    assert_eq!(adler32(b"A"), 0x0042_0042);
    assert!(
        zlib_decompress(INCOMPLETE, 1 << 20).is_err(),
        "incomplete literal/length code must be rejected, not decoded to Ok(b\"A\")"
    );
    let raw = &INCOMPLETE[2..INCOMPLETE.len() - 4];
    assert_eq!(inflate(raw, 1 << 20), Err(InflateError::BadSymbol));
}

#[test]
fn over_subscribed_huffman_rejected() {
    assert_eq!(
        Huffman::new(&[1, 1, 1]).err(),
        Some(InflateError::BadSymbol)
    );
    let (_h, left) = Huffman::new(&[1, 1]).unwrap();
    assert_eq!(left, 0);
    let (_h, left) = Huffman::new(&[0, 2]).unwrap();
    assert!(left > 0);
}

const INCOMPLETE_STREAM: &[u8] = &[
    0x78, 0x01, 0x05, 0xc0, 0x01, 0x09, 0x00, 0x00, 0x00, 0x80, 0xa0, 0x6d, 0xfd, 0x3f, 0x25, 0x01,
    0x00, 0x42, 0x00, 0x42,
];

const CORPUS: &[&[u8]] = &[EMPTY, HELLO, STORED, BACKREFS, DYNAMIC, INCOMPLETE_STREAM];

#[test]
fn root_table_resolves_every_fixed_literal_code() {
    let mut lit = fixed_lit();
    lit.arm_root();
    assert_eq!(lit.root.len(), ROOT_SIZE, "the root table must be built");
    assert!(
        lit.root.iter().all(|&e| e != 0),
        "a COMPLETE code whose longest word is ROOT_BITS must leave no empty \
         slot — an empty slot would silently route a fixed-block literal to \
         the fallback, and the table would be accelerating nothing"
    );
    for sym in 0u16..288 {
        let (code, len) = fixed_lit_code(sym);
        let bytes = bits_to_bytes(code, len);
        let mut via_table_reader = BitReader::new(&bytes);
        let mut via_walk_reader = BitReader::new(&bytes);
        let via_table = lit.decode(&mut via_table_reader);
        let via_walk = lit.decode_bitwise(&mut via_walk_reader);
        assert_eq!(via_table, Ok(sym), "table decode wrong for symbol {sym}");
        assert_eq!(via_table, via_walk, "paths disagree on symbol {sym}");
        assert_eq!(
            via_table_reader.bits_in_buffer, via_walk_reader.bits_in_buffer,
            "paths consumed a different number of bits for symbol {sym}"
        );
    }
}

#[test]
fn root_table_falls_back_for_codes_longer_than_root_bits() {
    let mut lengths = [0u16; 14];
    for (i, slot) in lengths.iter_mut().enumerate().take(12).skip(1) {
        *slot = u16::try_from(i).unwrap_or(0);
    }
    lengths[12] = 12;
    lengths[13] = 12;
    let (mut h, left) = Huffman::new(&lengths).expect("staircase code is valid");
    assert_eq!(left, 0, "staircase must be a complete code");
    h.arm_root();
    assert!(
        h.root.contains(&0),
        "a code with words longer than ROOT_BITS must leave empty slots"
    );
    for want in [12usize, 13] {
        let (code, len) = canonical_code_of(&lengths, want);
        assert!(len > ROOT_BITS, "symbol {want} must be a long code");
        let bytes = bits_to_bytes(code, len);
        let mut via_table_reader = BitReader::new(&bytes);
        let mut via_walk_reader = BitReader::new(&bytes);
        let want16 = u16::try_from(want).unwrap_or(0);
        assert_eq!(h.decode(&mut via_table_reader), Ok(want16));
        assert_eq!(h.decode_bitwise(&mut via_walk_reader), Ok(want16));
        assert_eq!(
            via_table_reader.bits_in_buffer,
            via_walk_reader.bits_in_buffer
        );
    }
}

#[test]
fn table_and_bitwise_decoders_agree_on_corpus_prefixes_and_mutations() {
    let mut checked = 0usize;
    let mut ok_seen = 0usize;
    let mut err_kinds = std::collections::BTreeSet::new();

    let mut compare = |input: &[u8], what: &str| {
        let raw: &[u8] = if input.len() > 6 {
            &input[2..input.len() - 4]
        } else {
            input
        };
        let table_z = zlib_decompress(input, 1 << 20);
        let walk_z = oracle::without_root_table(|| zlib_decompress(input, 1 << 20));
        assert_eq!(table_z, walk_z, "zlib_decompress diverged on {what}");
        let table_r = inflate(raw, 1 << 20);
        let walk_r = oracle::without_root_table(|| inflate(raw, 1 << 20));
        assert_eq!(table_r, walk_r, "inflate diverged on {what}");
        match &table_z {
            Ok(_) => ok_seen += 1,
            Err(e) => {
                err_kinds.insert(format!("{e:?}"));
            }
        }
        if let Err(e) = &table_r {
            err_kinds.insert(format!("{e:?}"));
        }
        checked += 1;
    };

    for (ci, corpus) in CORPUS.iter().enumerate() {
        compare(corpus, &format!("corpus[{ci}] whole"));
        for cut in 0..corpus.len() {
            compare(&corpus[..cut], &format!("corpus[{ci}] prefix {cut}"));
        }
        for pos in 0..corpus.len() {
            for mask in [0x01u8, 0x40, 0x80, 0xff] {
                let mut m = corpus.to_vec();
                m[pos] ^= mask;
                compare(&m, &format!("corpus[{ci}] byte {pos} ^ {mask:#04x}"));
            }
        }
    }

    assert!(
        checked > 1000,
        "the oracle must actually cover a wide space, only {checked} cases"
    );
    assert!(
        ok_seen > 0,
        "no case decoded successfully — corpus is broken"
    );
    for want in ["BadSymbol", "Truncated"] {
        assert!(
            err_kinds.contains(want),
            "the mutation space never produced {want}; it is not exercising \
             the paths the root table could diverge on. Saw: {err_kinds:?}"
        );
    }
}

fn fixed_lit_code(sym: u16) -> (u32, u32) {
    let mut lengths = [0u16; 288];
    for (i, len) in lengths.iter_mut().enumerate() {
        *len = match i {
            0..=143 => 8,
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        };
    }
    canonical_code_of(&lengths, sym as usize)
}

fn canonical_code_of(lengths: &[u16], want: usize) -> (u32, u32) {
    let mut count = [0u32; MAXBITS + 2];
    for &l in lengths {
        if l != 0 {
            count[l as usize] += 1;
        }
    }
    let mut next = [0u32; MAXBITS + 2];
    let mut code = 0u32;
    for len in 1..=MAXBITS {
        code = (code + count[len - 1]) << 1;
        next[len] = code;
    }
    let mut cursor = next;
    for (sym, &l) in lengths.iter().enumerate() {
        if l == 0 {
            continue;
        }
        let c = cursor[l as usize];
        cursor[l as usize] += 1;
        if sym == want {
            return (c, u32::from(l));
        }
    }
    (0, 0)
}

fn bits_to_bytes(code: u32, len: u32) -> Vec<u8> {
    let mut out = vec![0u8; 8];
    for i in 0..len {
        let bit = (code >> (len - 1 - i)) & 1;
        let idx = (i / 8) as usize;
        out[idx] |= u8::try_from(bit).unwrap_or(0) << (i % 8);
    }
    out
}

fn next(state: &mut u64) -> u32 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    (*state >> 33) as u32
}

#[test]
fn fuzz_never_panics_and_respects_cap() {
    const CAP: usize = 4096;
    let seeds: [&[u8]; 5] = [EMPTY, HELLO, STORED, BACKREFS, DYNAMIC];
    let mut state = 0x1234_5678_9abc_def0u64;
    for _ in 0..20_000 {
        let len = (next(&mut state) % 64) as usize;
        let mut buf: Vec<u8> = (0..len).map(|_| next(&mut state) as u8).collect();
        if next(&mut state) & 1 == 0 {
            let seed = seeds[(next(&mut state) as usize) % seeds.len()];
            let cut = (next(&mut state) as usize) % (seed.len() + 1);
            buf = seed[..cut].to_vec();
            for _ in 0..(next(&mut state) % 4) {
                if !buf.is_empty() {
                    let idx = (next(&mut state) as usize) % buf.len();
                    buf[idx] ^= (next(&mut state) as u8) | 1;
                }
            }
        }
        if let Ok(out) = zlib_decompress(&buf, CAP) {
            assert!(out.len() <= CAP, "zlib_decompress exceeded the cap");
        }
        if let Ok(out) = inflate(&buf, CAP) {
            assert!(out.len() <= CAP, "inflate exceeded the cap");
        }
    }
}

#[test]
fn every_strict_prefix_of_a_raw_stream_is_truncated() {
    for v in [EMPTY, HELLO, STORED, BACKREFS, DYNAMIC] {
        let raw = &v[2..v.len() - 4];
        assert!(inflate(raw, 1 << 20).is_ok());
        for cut in 0..raw.len() {
            assert_eq!(
                inflate(&raw[..cut], 1 << 20),
                Err(InflateError::Truncated),
                "cut {cut} of a {}-byte stream",
                raw.len()
            );
        }
    }
}

/// An LSB-first DEFLATE bit sink for the generated streams below.
#[derive(Default)]
struct Bits {
    out: Vec<u8>,
    acc: u32,
    held: u32,
}

impl Bits {
    fn put(&mut self, bits: u32, len: u32) {
        self.acc |= bits << self.held;
        self.held += len;
        while self.held >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.held -= 8;
        }
    }

    /// A Huffman code word, most significant bit first.
    fn code(&mut self, word: u32, len: u32) {
        self.put(reverse_code(word, len), len);
    }

    /// End the stream with a final stored block holding `tail`.
    fn finish(mut self, tail: &[u8]) -> Vec<u8> {
        self.put(0b001, 3); // BFINAL=1, BTYPE=00
        if self.held > 0 {
            self.put(0, 8 - self.held);
        }
        let len = u16::try_from(tail.len()).expect("a stored block holds 64 KiB");
        self.out.extend_from_slice(&len.to_le_bytes());
        self.out.extend_from_slice(&(!len).to_le_bytes());
        self.out.extend_from_slice(tail);
        self.out
    }
}

/// Raw DEFLATE: `blocks` empty fixed-Huffman blocks (10 bits each), then a
/// final stored block holding `tail`.
fn empty_fixed_blocks(blocks: usize, tail: &[u8]) -> Vec<u8> {
    let mut bits = Bits::default();
    for _ in 0..blocks {
        bits.put(0b010, 3); // BFINAL=0, BTYPE=01
        bits.put(0, 7); // end of block: fixed code 0000000
    }
    bits.finish(tail)
}

/// Raw DEFLATE: `blocks` empty dynamic blocks with the cheapest header to
/// write (12 bytes: literal 0 and end-of-block one bit each, no distance
/// code), then a final stored block holding `tail`.
fn empty_dynamic_blocks(blocks: usize, tail: &[u8]) -> Vec<u8> {
    // The code-length code: 18 one bit (word 0), 0 and 1 two bits (10, 11).
    let length_of = |symbol: usize| match symbol {
        18 => 1,
        0 | 1 => 2,
        _ => 0,
    };
    let mut bits = Bits::default();
    for _ in 0..blocks {
        bits.put(0b100, 3); // BFINAL=0, BTYPE=10
        bits.put(0, 5); // HLIT: 257 literal/length codes
        bits.put(0, 5); // HDIST: 1 distance code
        bits.put(14, 4); // HCLEN: 18 code-length codes
        for &symbol in CLCL_ORDER.iter().take(18) {
            bits.put(length_of(symbol), 3);
        }
        bits.code(0b11, 2); // literal 0: length 1
        bits.code(0, 1); // 18: 138 zeros (literals 1..=138)
        bits.put(138 - 11, 7);
        bits.code(0, 1); // 18: 117 zeros (literals 139..=255)
        bits.put(117 - 11, 7);
        bits.code(0b11, 2); // end of block: length 1
        bits.code(0b10, 2); // the one distance code: length 0
        bits.code(1, 1); // end of block
    }
    bits.finish(tail)
}

/// `raw` as a zlib stream whose output is `tail`.
fn zlib_of(raw: &[u8], tail: &[u8]) -> Vec<u8> {
    let mut zlib = vec![0x78, 0x01];
    zlib.extend_from_slice(raw);
    zlib.extend_from_slice(&adler32(tail).to_be_bytes());
    zlib
}

#[test]
fn fixed_blocks_decode_with_one_pair_of_tables() {
    use std::io::Read;
    const BLOCKS: usize = 20_000;
    const MEMBERS: usize = 1_000;
    let raw = empty_fixed_blocks(BLOCKS, b"tail");
    let zlib = zlib_of(&raw, b"tail");
    // An empty gzip member: a final empty fixed block, CRC-32 0, length 0.
    let member = [
        0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 3, 0x03, 0x00, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    let gzip = member.repeat(MEMBERS);

    let before = oracle::tables_built();
    assert_eq!(inflate(&raw, 1 << 20).as_deref(), Ok(&b"tail"[..]));
    let mut out = Vec::new();
    stream::DeflateReader::new(raw.as_slice())
        .read_to_end(&mut out)
        .expect("raw stream");
    assert_eq!(out, b"tail");
    out.clear();
    stream::ZlibReader::new(zlib.as_slice())
        .read_to_end(&mut out)
        .expect("zlib stream");
    assert_eq!(out, b"tail");
    out.clear();
    stream::GzipReader::new(gzip.as_slice())
        .read_to_end(&mut out)
        .expect("gzip stream");
    assert!(out.is_empty());
    let built = oracle::tables_built() - before;
    assert!(
        built <= 2,
        "{BLOCKS} empty fixed blocks, three ways, and {MEMBERS} gzip members built \
         {built} Huffman tables: a fixed block decodes with the one shared pair"
    );
}

#[test]
fn a_work_budget_refuses_block_headers_past_it_before_building_their_tables() {
    use std::io::Read;
    const BLOCKS: u64 = 1_000;
    let read = |zlib: &[u8], budget: u64| {
        let mut out = Vec::new();
        stream::ZlibReader::with_work_budget(zlib, budget)
            .read_to_end(&mut out)
            .map(|_| out)
    };
    let too_much = |e: std::io::Error| {
        e.kind() == std::io::ErrorKind::InvalidData
            && e.get_ref()
                .and_then(|e| e.downcast_ref::<InflateError>())
                .is_some_and(|e| *e == InflateError::TooMuchWork)
    };
    let blocks = usize::try_from(BLOCKS).expect("a usize");
    let dynamic = zlib_of(&empty_dynamic_blocks(blocks, b"tail"), b"tail");
    let fixed = zlib_of(&empty_fixed_blocks(blocks, b"tail"), b"tail");
    assert_eq!(
        inflate(&empty_dynamic_blocks(blocks, b"tail"), 1 << 20).as_deref(),
        Ok(&b"tail"[..]),
        "the generated dynamic blocks are well formed"
    );

    // Each stream costs its blocks plus the final stored one.
    let dynamic_cost = BLOCKS * DYNAMIC_HEADER_WORK + HEADER_WORK;
    let fixed_cost = BLOCKS * HEADER_WORK + HEADER_WORK;
    assert_eq!(
        read(&dynamic, dynamic_cost).ok().as_deref(),
        Some(&b"tail"[..])
    );
    assert_eq!(read(&fixed, fixed_cost).ok().as_deref(), Some(&b"tail"[..]));
    assert!(read(&dynamic, dynamic_cost - 1).is_err_and(too_much));
    assert!(read(&fixed, fixed_cost - 1).is_err_and(too_much));

    let before = oracle::tables_built();
    assert!(read(&dynamic, 10 * DYNAMIC_HEADER_WORK).is_err_and(too_much));
    assert_eq!(
        oracle::tables_built() - before,
        30,
        "ten dynamic headers built three tables each; the eleventh was refused first"
    );
}
