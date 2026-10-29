// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Inputs and helpers shared by the suite's tests.

mod sanity;
pub use sanity::sanity;

/// Seeded XorShift64 (13, 7, 17).
pub struct XorShift(u64);

impl XorShift {
    pub fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// Uniform in `0..n` (n > 0), near enough for corpus shaping.
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() >> 11) as usize % n
    }
}

/// Bytes a compressor cannot shrink.
pub fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut x = XorShift::new(seed);
    (0..len).map(|_| (x.next() >> 32) as u8).collect()
}

const WORDS: &[&str] = &[
    "the", "zstd", "frame", "block", "literal", "sequence", "offset", "window", "match", "length",
    "huffman", "table", "state", "decode", "aterm", "package", "install", "tree", "root", "sign",
    "fn", "let", "mut", "impl", "struct", "u8", "usize", "Result", "Ok", "Err", "return", "self",
    "{", "}", "(", ")", ";", "=", "->", "&", "::", "//",
];

/// Seeded source-like text: words, numbers and line breaks. Compresses to
/// Huffman literals and short matches with repeat offsets.
pub fn words(len: usize, seed: u64) -> Vec<u8> {
    let mut x = XorShift::new(seed);
    let mut out = Vec::with_capacity(len + 16);
    while out.len() < len {
        match x.below(16) {
            0 => out.push(b'\n'),
            1 => out.extend_from_slice(format!("{}", x.below(100_000)).as_bytes()),
            2 => out.extend_from_slice(b"    "),
            _ => out.extend_from_slice(WORDS[x.below(WORDS.len())].as_bytes()),
        }
        out.push(b' ');
    }
    out.truncate(len);
    out
}

/// A seeded mix shaped like an archive of a toolchain: runs of text, noise,
/// one repeated byte, little-endian counters (structured binary), and copies
/// of earlier spans from near and far. Exercises every block type, every
/// literals type, the repeat offsets and long matches.
pub fn mixed(len: usize, seed: u64) -> Vec<u8> {
    let mut x = XorShift::new(seed);
    let mut out = Vec::with_capacity(len + 70_000);
    while out.len() < len {
        let span = 1 + x.below(40_000);
        match x.below(6) {
            0 => out.extend(words(span, x.next())),
            1 => out.extend(noise(span.min(5_000), x.next())),
            2 => {
                let byte = x.next() as u8;
                out.extend(std::iter::repeat_n(byte, span));
            }
            3 => {
                let mut n = x.next() as u32;
                for _ in 0..span / 4 {
                    out.extend(n.to_le_bytes());
                    n = n.wrapping_add(1 + (x.below(3) as u32));
                }
            }
            _ if !out.is_empty() => {
                let from = x.below(out.len());
                let n = span.min(out.len() - from);
                out.extend_from_within(from..from + n);
            }
            _ => {}
        }
    }
    out.truncate(len);
    out
}

/// A seeded 20,000-byte run of [`words`] repeated, a counter between copies:
/// what `pzstd` cuts into several independent frames that each compress well.
pub fn repetitive(len: usize, seed: u64) -> Vec<u8> {
    let unit = words(20_000, seed);
    let mut out = Vec::with_capacity(len + unit.len() + 64);
    let mut i = 0u64;
    while out.len() < len {
        out.extend_from_slice(&unit);
        out.extend_from_slice(i.to_string().repeat(3).as_bytes());
        i += 1;
    }
    out.truncate(len);
    out
}

/// A block of sequences that all share one code of each kind, every code
/// with extra bits, so an encoder writes RLE-mode tables for it: units of 16
/// noise literals and a 40-byte copy from a fresh distance in 125..=252
/// (literal length code 16, match length code 34, offset code 7, never a
/// repeat offset), `units` of them. The bytes either side of each copy
/// differ from those either side of its source, so no match grows past 40.
pub fn one_code(units: usize, seed: u64) -> Vec<u8> {
    let mut x = XorShift::new(seed);
    let mut out = vec![0; (128 << 10) - 256];
    out.extend(noise(256, seed));
    let mut recent = [0usize; 3];
    let mut after_source = None;
    for _ in 0..units {
        let distance = loop {
            let d = 125 + x.below(128);
            if !recent.contains(&d) {
                break d;
            }
        };
        recent = [distance, recent[0], recent[1]];
        for i in 0..16 {
            let mut byte = x.next() as u8;
            let avoid = match i {
                0 => after_source,
                15 => Some(out[out.len() - distance]),
                _ => None,
            };
            if avoid == Some(byte) {
                byte = byte.wrapping_add(1);
            }
            out.push(byte);
        }
        let from = out.len() - distance;
        out.extend_from_within(from..from + 40);
        after_source = Some(out[from + 40]);
    }
    out
}

/// A block whose literals are all one byte: after the same first block as
/// [`one_code`], `units` of one `x` and a 40-byte copy from a fresh distance
/// in 125..=252. No source starts just after an `x` or ends just before one,
/// so no match takes an `x` in: the block's literals are `units` `x`s, which
/// an encoder writes as one RLE literals section.
pub fn rle_literals(units: usize, seed: u64) -> Vec<u8> {
    let mut x = XorShift::new(seed);
    let mut out = vec![0; (128 << 10) - 256];
    out.extend(
        noise(256, seed)
            .into_iter()
            .map(|b| if b == b'x' { b'y' } else { b }),
    );
    for _ in 0..units {
        out.push(b'x');
        let from = loop {
            let from = out.len() - (125 + x.below(128));
            if out[from - 1] != b'x' && out[from + 40] != b'x' {
                break from;
            }
        };
        out.extend_from_within(from..from + 40);
    }
    out
}

/// A seeded 16 KiB of noise, 9 MiB of zeros, then the noise again: the
/// repeat is a match 9 MiB back, past the 8 MiB window of `zstd -19`, which
/// only a frame with a larger window (`--long=27`) can reach.
pub fn far_repeat(seed: u64) -> Vec<u8> {
    let head = noise(16 * 1024, seed);
    let mut out = head.clone();
    out.resize(head.len() + (9 << 20), 0);
    out.extend_from_slice(&head);
    out
}

/// Rounds of three 128 KiB blocks: noise; noise with two 4-byte copies from
/// 125,000 and 130,000 back, which cost more than they save (an encoder codes
/// them, then writes the block raw); then 16 noise bytes, 32 bytes copied
/// from 130,000 back, and text. A raw block moves no repeat offset and sets
/// no table (RFC 8878 §3.1.1.2), so an encoder that kept its state would name
/// the third block's copy by a repeat offset the decoder does not hold, or
/// repeat a table the decoder never read.
pub fn raw_after_coding(rounds: usize, seed: u64) -> Vec<u8> {
    const BLOCK: usize = 128 << 10;
    let mut out = Vec::with_capacity(rounds * 3 * BLOCK);
    for r in 0..rounds as u64 {
        let at = out.len();
        out.extend(noise(2 * BLOCK + 16, seed + 2 * r));
        for (copy, dist) in [(at + BLOCK + 40, 125_000), (at + BLOCK + 150, 130_000)] {
            out.copy_within(copy - dist..copy - dist + 4, copy);
        }
        let from = out.len() - 130_000;
        out.extend_from_within(from..from + 32);
        out.extend(words(BLOCK - 48, seed + 2 * r));
    }
    out
}

/// 32,768 distinct 4-byte words, each followed by a separator byte no word
/// holds, zeros to the next 128 KiB, then a block of the same words in another
/// order. Each word of that block matches its first copy and nothing longer,
/// so with tables that reach that far back the block is 32,768 sequences of
/// one 4-byte match and no literal: more than the 32,511 a two-byte
/// `Number_of_Sequences` holds (RFC 8878 §3.1.1.3.2.1).
pub fn many_sequences(seed: u64) -> Vec<u8> {
    const N: usize = 32_768;
    const SEPARATOR: u8 = 0xFF;
    let mut x = XorShift::new(seed);
    let mut seen = std::collections::HashSet::with_capacity(N);
    let mut list = Vec::with_capacity(N);
    while list.len() < N {
        let word = (x.next() >> 16) as u32;
        if !word.to_le_bytes().contains(&SEPARATOR) && seen.insert(word) {
            list.push(word.to_le_bytes());
        }
    }
    let mut out = Vec::with_capacity(3 * N * 4);
    for word in &list {
        out.extend_from_slice(word);
        out.push(SEPARATOR);
    }
    out.resize(2 * N * 4, 0);
    // Fisher-Yates.
    for i in (1..N).rev() {
        list.swap(i, x.below(i + 1));
    }
    for word in &list {
        out.extend_from_slice(word);
    }
    out
}

/// A seeded 16 KiB of noise, then zeros, its four 4 KiB quarters copied
/// 2^24 + 9, 2^25 + 5, 2^26 + 77 and 2^27 - 1 bytes on from where each sits:
/// under a 2^27 window, matches of offset codes 24, 25, 26 and 27 (RFC 8878
/// §3.1.1.3.2.1.1), which the predefined offset table holds as "less than 1"
/// probabilities (§3.1.1.3.2.2.3); the last is as far as a frame of this
/// encoder reaches.
pub fn far_offsets(seed: u64) -> Vec<u8> {
    const QUARTER: usize = 4096;
    let dists = [(1 << 24) + 9, (1 << 25) + 5, (1 << 26) + 77, (1 << 27) - 1];
    let mut out = noise(4 * QUARTER, seed);
    out.resize(3 * QUARTER + (1 << 27) - 1 + QUARTER, 0);
    for (q, d) in dists.into_iter().enumerate() {
        let from = q * QUARTER;
        out.copy_within(from..from + QUARTER, from + d);
    }
    out
}

/// A 64-byte pattern repeated, one `x` inserted at a seeded point in each
/// copy: past the first copy, matches cover everything but the `x`s, so a
/// block that starts past it has one-byte (RLE) literals.
pub fn inserts(len: usize, seed: u64) -> Vec<u8> {
    let pattern = noise(64, seed);
    let mut x = XorShift::new(seed);
    let mut out = pattern.clone();
    while out.len() < len {
        let k = x.below(64);
        out.extend_from_slice(&pattern[..k]);
        out.push(b'x');
        out.extend_from_slice(&pattern[k..]);
    }
    out.truncate(len);
    out
}

/// Zeros with one seeded byte every 4 KiB: compressed blocks of a few short
/// sequences (one literal, a repeat-offset match) at any length, so sizes
/// across the block boundary cost a few hundred bytes recorded.
pub fn sparse(len: usize, seed: u64) -> Vec<u8> {
    let mut x = XorShift::new(seed);
    let mut out = vec![0; len];
    for at in (0..len).step_by(4096) {
        out[at] = x.next() as u8 | 1;
    }
    out
}

/// The input a recorded row names, `kind:len:seed`: `words`, `inserts`,
/// `sparse`, `noise`, `one_code` and `rle_literals` (`len` counts their
/// units) or `run` (`len` bytes of `z`, no seed used); and `text:0:<ascii>`,
/// those bytes as written.
pub fn input(spec: &str) -> Vec<u8> {
    let mut parts = spec.splitn(3, ':');
    let (kind, len, seed) = (
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
    );
    if kind == "text" {
        return seed.as_bytes().to_vec();
    }
    let len: usize = len.parse().unwrap_or_else(|_| panic!("{spec}: length"));
    let seed: u64 = seed.parse().unwrap_or_else(|_| panic!("{spec}: seed"));
    match kind {
        "words" => words(len, seed),
        "inserts" => inserts(len, seed),
        "sparse" => sparse(len, seed),
        "noise" => noise(len, seed),
        "one_code" => one_code(len, seed),
        "rle_literals" => rle_literals(len, seed),
        "run" => vec![b'z'; len],
        _ => panic!("{spec}: no such input"),
    }
}

/// The recorded level sweep (`recorded/sweep-zstd-1.5.7.tsv` says how it was
/// made): every frame back to back.
pub const SWEEP: &[u8] = include_bytes!("../recorded/sweep-zstd-1.5.7.zst");
const SWEEP_ROWS: &str = include_str!("../recorded/sweep-zstd-1.5.7.tsv");

#[derive(Debug)]
pub struct SweepRow {
    pub row: usize,
    pub offset: usize,
    pub len: usize,
    /// The input's `kind:len:seed`, for [`input`].
    pub input: String,
    /// The CLI command that wrote the frame.
    pub command: String,
    pub decoded_len: usize,
    pub sha256: String,
}

impl SweepRow {
    pub fn frame(&self) -> &'static [u8] {
        &SWEEP[self.offset..self.offset + self.len]
    }
}

/// Every sweep row, in order; the rows tile [`SWEEP`] exactly.
pub fn sweep() -> Vec<SweepRow> {
    let rows: Vec<SweepRow> = SWEEP_ROWS
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
        .map(|l| {
            let c: Vec<&str> = l.split('\t').collect();
            assert_eq!(c.len(), 7, "{l}");
            let num = |s: &str| s.parse::<usize>().unwrap_or_else(|_| panic!("{l}"));
            SweepRow {
                row: num(c[0]),
                offset: num(c[1]),
                len: num(c[2]),
                input: c[3].to_string(),
                command: c[4].to_string(),
                decoded_len: num(c[5]),
                sha256: c[6].to_string(),
            }
        })
        .collect();
    let mut at = 0;
    for (i, r) in rows.iter().enumerate() {
        assert_eq!((r.row, r.offset), (i, at), "sweep row {i}");
        at += r.len;
    }
    assert_eq!(at, SWEEP.len(), "the rows tile the recorded frames");
    rows
}

/// The sweep rows the bit-flip lane damages: the 1,536-byte words frames at
/// --fast=5, -1, -5, -12, -17 and --ultra -21 (one per match-finder family),
/// each written with a checksum, so it is flipped with and without one.
pub const FLIP_ROWS: [usize; 6] = [0, 21, 37, 68, 91, 109];

/// The seed of the generator that picks the flipped bits, run through the
/// flip frames in order: each [`FLIP_ROWS`] frame as recorded, then without
/// its checksum.
pub const FLIP_SEED: u64 = 0xF1_1B;

/// A checksummed frame as the CLI writes it with `--no-check`: the same
/// blocks, `Content_Checksum_flag` clear and the four checksum bytes gone.
pub fn without_checksum(frame: &[u8]) -> Vec<u8> {
    assert_ne!(frame[4] & 0x04, 0, "the frame carries a checksum");
    let mut out = frame[..frame.len() - 4].to_vec();
    out[4] &= !0x04;
    out
}

/// The bits flipped in one frame of `len` bytes: every bit of the first 64
/// bytes (headers and tables), then 400 drawn from `x`.
pub fn flip_bits(len: usize, x: &mut XorShift) -> Vec<usize> {
    let mut bits: Vec<usize> = (0..(64 * 8).min(len * 8)).collect();
    bits.extend((0..400).map(|_| x.below(len * 8)));
    bits
}

const FRAME_MAGIC: u32 = 0xFD2F_B528;

#[derive(Debug, Default)]
pub struct Census {
    pub frames: usize,
    pub skippable: usize,
    pub single_segment: usize,
    pub with_checksum: usize,
    pub with_content_size: usize,
    pub max_window_log: u8,
    pub blocks: [usize; 3],
    pub literals: [usize; 4],
    pub modes: [usize; 4],
    /// RLE-mode tables whose one code carries extra bits, by kind
    /// `[literal length, offset, match length]`: the table's one entry must
    /// still read them (counted where no FSE description comes first, as
    /// this walker does not parse one).
    pub rle_extra_bits: [usize; 3],
    /// Compressed blocks with sequences, by the width of their
    /// `Number_of_Sequences` field (1, 2 or 3 bytes, RFC 8878 §3.1.1.3.2.1).
    pub sequence_counts: [usize; 3],
}

impl Census {
    pub fn add(&mut self, zst: &[u8]) {
        let le = |b: &[u8]| b.iter().rev().fold(0u64, |acc, &x| acc << 8 | u64::from(x));
        let mut z = zst;
        while !z.is_empty() {
            let magic = le(&z[..4]) as u32;
            if (0x184D_2A50..=0x184D_2A5F).contains(&magic) {
                self.skippable += 1;
                z = &z[8 + le(&z[4..8]) as usize..];
                continue;
            }
            assert_eq!(magic, FRAME_MAGIC);
            self.frames += 1;
            let fhd = z[4];
            let single = fhd & 0x20 != 0;
            let mut pos = 5;
            if single {
                self.single_segment += 1;
            } else {
                self.max_window_log = self.max_window_log.max(10 + (z[5] >> 3));
                pos += 1;
            }
            pos += [0, 1, 2, 4][usize::from(fhd & 3)];
            let fcs = [usize::from(single), 2, 4, 8][usize::from(fhd >> 6)];
            self.with_content_size += usize::from(fcs > 0);
            pos += fcs;
            loop {
                let header = le(&z[pos..pos + 3]) as usize;
                pos += 3;
                let size = header >> 3;
                match (header >> 1) & 3 {
                    0 => {
                        self.blocks[0] += 1;
                        pos += size;
                    }
                    1 => {
                        self.blocks[1] += 1;
                        pos += 1;
                    }
                    _ => {
                        self.blocks[2] += 1;
                        self.compressed_block(&z[pos..pos + size]);
                        pos += size;
                    }
                }
                if header & 1 == 1 {
                    break;
                }
            }
            if fhd & 4 != 0 {
                self.with_checksum += 1;
                pos += 4;
            }
            z = &z[pos..];
        }
    }

    fn compressed_block(&mut self, b: &[u8]) {
        let kind = usize::from(b[0] & 3);
        self.literals[kind] += 1;
        let sf = (b[0] >> 2) & 3;
        let byte = |i: usize| usize::from(b[i]);
        let (header, body) = if kind < 2 {
            let (h, regenerated) = match sf {
                0 | 2 => (1, byte(0) >> 3),
                1 => (2, (byte(0) >> 4) + (byte(1) << 4)),
                _ => (3, (byte(0) >> 4) + (byte(1) << 4) + (byte(2) << 12)),
            };
            (h, if kind == 0 { regenerated } else { 1 })
        } else {
            match sf {
                0 | 1 => (3, (byte(1) >> 6) + (byte(2) << 2)),
                2 => (4, (byte(2) >> 2) + (byte(3) << 6)),
                _ => (5, (byte(2) >> 6) + (byte(3) << 2) + (byte(4) << 10)),
            }
        };
        let s = &b[header + body..];
        let (modes_at, modes) = match s[0] {
            0 => return,
            1..=127 => (1, s[1]),
            128..=254 if s[1] == 0 && s[0] == 128 => return,
            128..=254 => (2, s[2]),
            255 => (3, s[3]),
        };
        self.sequence_counts[modes_at - 1] += 1;
        let table_modes = [6, 4, 2].map(|shift| usize::from((modes >> shift) & 3));
        for mode in table_modes {
            self.modes[mode] += 1;
        }
        let mut at = modes_at + 1;
        for (kind, mode) in table_modes.into_iter().enumerate() {
            match mode {
                1 => {
                    let first_extra = [16, 1, 32][kind];
                    self.rle_extra_bits[kind] += usize::from(s[at] >= first_extra);
                    at += 1;
                }
                2 => break,
                _ => {}
            }
        }
    }
}

/// `f` of each of `items`, on every core, in `items`' order: a sweep's cuts
/// and flips are independent decodes.
pub fn on_every_core<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let next = AtomicUsize::new(0);
    let done = std::sync::Mutex::new(Vec::with_capacity(items.len()));
    let threads = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
    std::thread::scope(|scope| {
        for _ in 0..threads.min(items.len()) {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(i) else {
                        break;
                    };
                    let r = f(item);
                    done.lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push((i, r));
                }
            });
        }
    });
    let mut done = done
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    done.sort_by_key(|(i, _)| *i);
    done.into_iter().map(|(_, r)| r).collect()
}
