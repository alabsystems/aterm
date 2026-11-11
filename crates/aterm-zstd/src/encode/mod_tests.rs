// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::io::{Read, Write};

use super::{
    Encoder, LEVEL_DEFAULT, LEVEL_MAX, Options, WINDOW_LOG_MAX, WINDOW_LOG_MIN, write_header,
};
use crate::Decoder;
use crate::decode::header;
use crate::frame::Checksum;

/// Text from a small vocabulary: matches near and far, at every distance a window holds.
fn text(len: usize, seed: u64) -> Vec<u8> {
    const WORDS: [&[u8]; 12] = [
        b"frame ",
        b"block ",
        b"window ",
        b"offset ",
        b"literal ",
        b"match ",
        b"table ",
        b"state ",
        b"weight ",
        b"stream ",
        b"repeat ",
        b"\n",
    ];
    let mut x = seed | 1;
    let mut out = Vec::with_capacity(len + 16);
    while out.len() < len {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        out.extend_from_slice(WORDS[(x % 12) as usize]);
        if x.is_multiple_of(97) {
            out.extend_from_slice(&x.to_le_bytes());
        }
    }
    out.truncate(len);
    out
}

fn decode(zst: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    Decoder::with_buffer(zst).read_to_end(&mut out).unwrap();
    out
}

/// Bytes no match shrinks.
fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 32) as u8
        })
        .collect()
}

const BLOCK: usize = 128 << 10;

/// Three blocks: noise; noise with two 4-byte copies from 125,000 and 130,000 back,
/// which cost more than they save, so the block is coded and then written raw; then 16
/// noise bytes, 32 bytes copied from 130,000 back, and text. An encoder that kept the
/// raw block's repeat offsets would name the third block's copy by one the decoder
/// does not hold.
fn raw_after_coding() -> Vec<u8> {
    let mut h = noise(2 * BLOCK + 16, 0x5EED);
    for (at, dist) in [(BLOCK + 40, 125_000), (BLOCK + 150, 130_000)] {
        h.copy_within(at - dist..at - dist + 4, at);
    }
    let from = h.len() - 130_000;
    h.extend_from_within(from..from + 32);
    h.extend(text(BLOCK - 48, 3));
    h
}

/// [`raw_after_coding`] at levels 3 and 9 (level 1's hash table is too small to keep the
/// far copies' sources), streamed a block at a time with its size declared, undeclared,
/// and undeclared with a flush after each block, the rollback's fault switched on when
/// `keep_rejected`; each frame with its count of blocks coded, then written raw.
fn raw_after_coding_frames(keep_rejected: bool) -> Vec<(String, Vec<u8>, usize)> {
    let input = raw_after_coding();
    let mut out = Vec::new();
    for level in [3, 9] {
        for (declared, flush) in [(true, false), (false, false), (false, true)] {
            let mut options = Options::new().level(level);
            if declared {
                options = options.content_size(input.len() as u64);
            }
            let mut enc = Encoder::new(Vec::new(), options);
            enc.blocks.rollback.keep_rejected = keep_rejected;
            for p in input.chunks(BLOCK) {
                enc.write_all(p).unwrap();
                if flush {
                    enc.flush().unwrap();
                }
            }
            let rejected = enc.blocks.rollback.rejected;
            let what = format!("level {level} declared {declared} flush {flush}");
            out.push((what, enc.finish().unwrap(), rejected));
        }
    }
    out
}

/// A raw block carries no table and moves no repeat offset (RFC 8878 §3.1.1.2), so a
/// block coded and then written raw leaves the entropy state as the decoder holds it.
#[test]
fn a_block_written_raw_leaves_the_decoder_state_alone() {
    let input = raw_after_coding();
    for (what, zst, rejected) in raw_after_coding_frames(false) {
        assert!(rejected >= 1, "{what}: a block was coded, then written raw");
        assert!(decode(&zst) == input, "{what}");
    }
}

/// The control: keeping the raw block's state (the fault the rollback prevents) gives
/// frames the decoder refuses or decodes to other bytes.
#[test]
fn keeping_a_raw_block_s_state_breaks_the_frame() {
    let input = raw_after_coding();
    for (what, zst, rejected) in raw_after_coding_frames(true) {
        assert!(rejected >= 1, "{what}");
        let mut back = Vec::new();
        let read = Decoder::with_buffer(&zst[..]).read_to_end(&mut back);
        assert!(
            read.is_err() || back != input,
            "{what}: the fault went unseen"
        );
    }
}

/// Streams `input` in writes of `piece` through a 1 KiB window, rebasing positions past
/// `rebase_past`; returns the frame and the encoder's last base.
fn streamed(input: &[u8], level: u8, piece: usize, rebase_past: u32) -> (Vec<u8>, u32) {
    let mut enc = Encoder::new(Vec::new(), Options::new().level(level).window_log(10));
    enc.rebase_past = rebase_past;
    for p in input.chunks(piece) {
        enc.write_all(p).unwrap();
    }
    let base = enc.base;
    (enc.finish().unwrap(), base)
}

/// Positions move down many times over a long stream, and the frame still decodes:
/// the match finder's tables and the buffer's base move together.
#[test]
fn a_stream_past_many_rebases_decodes() {
    let input = text(3 << 20, 7);
    for level in [1, 5, 19] {
        let (zst, base) = streamed(&input, level, 7919, 1 << 14);
        assert!(base < 1 << 15, "level {level}: base {base} was moved down");
        assert!(
            zst.len() < input.len() / 2,
            "level {level}: it still compresses"
        );
        assert!(decode(&zst) == input, "level {level}");
    }
}

/// The control: at the default threshold the same stream never rebases, and its frame
/// is the rebased one's byte for byte.
#[test]
fn below_the_threshold_nothing_rebases() {
    let input = text(3 << 20, 7);
    let (rebased, _) = streamed(&input, 5, 7919, 1 << 14);
    let (plain, base) = streamed(&input, 5, 7919, super::REBASE_PAST);
    assert!(base > (3 << 20) - 8192, "base {base} grew with the input");
    assert!(plain == rebased, "a rebase changes no match");
}

/// With a chain ring wider than a block (2^20 links at level 7 under a 2^20 window) and
/// flushes cutting blocks at odd places, the buffer slides by amounts that are not
/// multiples of the ring, yet each rebase moves positions by a multiple of it, so the
/// rebased frame is the plain one byte for byte. Under the 1 KiB window above, ring,
/// block and window are equal and every slide is already a multiple.
#[test]
fn a_rebase_past_a_wide_ring_changes_no_match() {
    let input = text(6 << 20, 13);
    let run = |rebase_past: u32| {
        let mut enc = Encoder::new(Vec::new(), Options::new().level(7).window_log(20));
        enc.rebase_past = rebase_past;
        let mut rebases = 0;
        let mut moved = 0;
        for p in input.chunks(100_003) {
            enc.write_all(p).unwrap();
            enc.flush().unwrap();
            // Unmoved, hist[0]'s position is 1 plus the bytes dropped before it.
            let now = enc.total as usize - enc.hist.len() + 1 - enc.base as usize;
            assert_eq!(now % (1 << 20), 0, "moved by {now}");
            rebases += usize::from(now > moved);
            moved = now;
        }
        let ring = enc.matcher.as_ref().map(super::Finder::ring_len);
        assert_eq!(ring, Some(1 << 20), "the ring is wider than a block");
        (enc.finish().unwrap(), rebases)
    };
    let (rebased, rebases) = run(1 << 20);
    let (plain, none) = run(super::REBASE_PAST);
    assert!(rebases >= 2 && none == 0, "{rebases} rebases, {none}");
    assert!(rebased == plain, "a rebase changes no match");
    assert!(decode(&rebased) == input);
}

/// Whatever the writes, the buffer holds at most two windows and two blocks, and it
/// does fill past half of that.
#[test]
fn the_buffer_stays_within_its_bound() {
    let input = text(8 << 20, 11);
    for (window_log, pieces) in [(16u8, [1000, 1 << 20]), (20, [65_537, 3 << 20])] {
        for piece in pieces {
            let mut enc = Encoder::new(Vec::new(), Options::new().level(3).window_log(window_log));
            let cap = enc.cap();
            let mut most = 0;
            for p in input.chunks(piece) {
                enc.write_all(p).unwrap();
                most = most.max(enc.hist.capacity());
                assert!(
                    enc.hist.capacity() <= cap,
                    "{window_log} {piece}: {most} > {cap}"
                );
            }
            assert!(
                most > cap / 2,
                "{window_log} {piece}: the buffer filled ({most} of {cap})"
            );
            assert!(
                decode(&enc.finish().unwrap()) == input,
                "{window_log} {piece}"
            );
        }
    }
}

/// Every header shape reads back through the decoder's header parser.
#[test]
fn headers_read_back() {
    for (log, size, single) in [
        (10u8, None, false),
        (23, None, false),
        (27, Some(5u64 << 30), false),
        (17, Some(300), false),
        (20, Some(0), true),
        (20, Some(255), true),
        (20, Some(256), true),
        (20, Some(65_791), true),
        (20, Some(65_792), true),
        (27, Some(1 << 27), true),
    ] {
        for checksum in [Checksum::Xxh64, Checksum::Omitted] {
            let mut out = Vec::new();
            write_header(&mut out, log, size, single, checksum);
            let h = header::read(&mut &out[4..]).unwrap();
            assert_eq!(h.content_size, size, "{log} {size:?} {single}");
            assert_eq!(h.checksum, checksum == Checksum::Xxh64);
            let window = if single {
                size.unwrap().max(1024) as usize
            } else {
                1 << log
            };
            assert_eq!(h.window, window);
        }
    }
}

/// The negative control: a single-segment header read as if windowed differs.
#[test]
fn a_header_with_its_segment_bit_cleared_reads_otherwise() {
    let mut out = Vec::new();
    write_header(&mut out, 20, Some(300), true, Checksum::Xxh64);
    out[4] &= !0x20;
    let h = header::read(&mut &out[4..]);
    assert!(h.is_err() || h.unwrap().content_size != Some(300));
}

#[test]
fn options_clamp_to_valid_bounds() {
    let def = Options::default();
    assert_eq!(def, Options::new());
    assert_eq!(def.level, LEVEL_DEFAULT);
    assert_eq!(def.window_log, None);
    assert_eq!(def.checksum, Checksum::Xxh64);
    assert_eq!(def.content_size, None);

    assert_eq!(Options::new().level(0).level, 1);
    assert_eq!(Options::new().level(1).level, 1);
    assert_eq!(Options::new().level(LEVEL_MAX).level, LEVEL_MAX);
    assert_eq!(Options::new().level(u8::MAX).level, LEVEL_MAX);

    assert_eq!(
        Options::new().window_log(0).window_log,
        Some(WINDOW_LOG_MIN)
    );
    assert_eq!(
        Options::new().window_log(WINDOW_LOG_MIN).window_log,
        Some(WINDOW_LOG_MIN)
    );
    assert_eq!(
        Options::new().window_log(WINDOW_LOG_MAX).window_log,
        Some(WINDOW_LOG_MAX)
    );
    assert_eq!(
        Options::new().window_log(u8::MAX).window_log,
        Some(WINDOW_LOG_MAX)
    );

    let custom = Options::new()
        .checksum(Checksum::Omitted)
        .content_size(4096);
    assert_eq!(custom.level, LEVEL_DEFAULT);
    assert_eq!(custom.window_log, None);
    assert_eq!(custom.checksum, Checksum::Omitted);
    assert_eq!(custom.content_size, Some(4096));
}
