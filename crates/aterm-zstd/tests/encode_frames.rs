// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! [`aterm_zstd::encode`] held to the crate's decoder: its frames across levels, windows,
//! checksum choices and ways of feeding it decode to their input, and together use every
//! part of the format a compressed frame can carry.

use std::io::{self, Read, Write};

use crate::support::{self, Census, on_every_core};
use aterm_zstd::encode::{self, Encoder, Options};
use aterm_zstd::frame::Checksum;
use aterm_zstd::{DecodeError, Decoder};

pub fn decode(zst: &[u8]) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    Decoder::with_buffer(zst).read_to_end(&mut out)?;
    Ok(out)
}

/// The generated inputs: the shapes the format has a tool for, at the sizes where the
/// encoder's choices change (a block's 128 KiB, a window's edge).
pub fn corpus() -> Vec<(String, Vec<u8>)> {
    let mut c: Vec<(String, Vec<u8>)> = vec![("empty".into(), Vec::new())];
    for len in [1, 2, 3, 4, 5, 7, 8, 15, 16, 31, 32, 33, 63, 64] {
        c.push((format!("noise:{len}"), support::noise(len, len as u64)));
        c.push((format!("words:{len}"), support::words(len, len as u64)));
    }
    for len in [1, 2, 100, 4096, 128 << 10, (128 << 10) + 1, 300_000] {
        c.push((format!("one-byte:{len}"), vec![0xA5; len]));
    }
    c.extend([
        ("noise:1k".into(), support::noise(1000, 1)),
        ("noise:200k".into(), support::noise(200_000, 2)),
        ("words:1k".into(), support::words(1000, 3)),
        ("words:100k".into(), support::words(100_000, 4)),
        ("words:400k".into(), support::words(400_000, 5)),
        ("mixed:1m".into(), support::mixed(1 << 20, 6)),
        ("repetitive:600k".into(), support::repetitive(600_000, 7)),
        ("sparse:500k".into(), support::sparse(500_000, 8)),
        ("inserts:300k".into(), support::inserts(300_000, 9)),
        ("one_code:24".into(), support::one_code(24, 1)),
        ("rle_literals:64".into(), support::rle_literals(64, 2)),
        ("sanity:200k".into(), support::sanity(200_000)),
        (
            "raw-after-coding:768k".into(),
            support::raw_after_coding(2, 61),
        ),
        ("many-sequences:384k".into(), support::many_sequences(62)),
    ]);
    // A long repeat past several blocks: a 5000-byte unit, then its copies.
    let unit = support::words(5000, 10);
    c.push(("long-repeat:700k".into(), unit.repeat(140)));
    c
}

/// The sweep's and the recorded CLI frames' inputs, each once.
fn recorded_inputs() -> Vec<(String, Vec<u8>)> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for r in support::sweep() {
        if seen.insert(r.input.clone()) {
            out.push((format!("sweep:{}", r.input), support::input(&r.input)));
        }
    }
    out.extend([
        ("cli:words-96k".into(), support::words(96 * 1024, 1)),
        ("cli:words-32k".into(), support::words(32 * 1024, 1)),
        ("cli:words-48k".into(), support::words(48 * 1024, 1)),
        ("cli:mixed-300k".into(), support::mixed(300 * 1024, 2)),
        (
            "cli:repetitive-2700k".into(),
            support::repetitive(2_700_000, 1),
        ),
        ("cli:one-code".into(), support::one_code(24, 1)),
    ]);
    out
}

/// One way to write a frame.
#[derive(Clone, Copy, Debug)]
pub enum Feed {
    /// [`encode::compress`].
    OneShot,
    /// [`Encoder`] in writes of this many bytes, size undeclared.
    Chunks(usize),
    /// Writes of this many bytes, a [`Write::flush`] after each.
    Flushed(usize),
}

pub fn write_frame(input: &[u8], options: Options, feed: Feed) -> Vec<u8> {
    match feed {
        Feed::OneShot => encode::compress(input, &options),
        Feed::Chunks(n) | Feed::Flushed(n) => {
            let mut enc = Encoder::new(Vec::new(), options);
            for piece in input.chunks(n.max(1)) {
                enc.write_all(piece).expect("write");
                if matches!(feed, Feed::Flushed(_)) {
                    enc.flush().expect("flush");
                }
            }
            enc.finish().expect("finish")
        }
    }
}

struct Case {
    name: String,
    level: u8,
    window_log: Option<u8>,
    checksum: Checksum,
    feed: Feed,
}

fn cases(inputs: &[(String, Vec<u8>)]) -> Vec<(usize, Case)> {
    let mut cases = Vec::new();
    for (i, (name, input)) in inputs.iter().enumerate() {
        let big = input.len() > 150_000;
        let levels: &[u8] = if big {
            &[1, 3, 12, 19]
        } else {
            &[1, 2, 3, 5, 9, 15, 19]
        };
        for &level in levels {
            let checksum = if level % 2 == 0 {
                Checksum::Omitted
            } else {
                Checksum::Xxh64
            };
            cases.push((
                i,
                Case {
                    name: name.clone(),
                    level,
                    window_log: None,
                    checksum,
                    feed: Feed::OneShot,
                },
            ));
        }
        let feeds: &[Feed] = if big {
            &[Feed::Chunks(65_537), Feed::Flushed(300_001)]
        } else {
            &[
                Feed::Chunks(1),
                Feed::Chunks(7),
                Feed::Chunks(4096),
                Feed::Flushed(1000),
            ]
        };
        for &feed in feeds {
            cases.push((
                i,
                Case {
                    name: name.clone(),
                    level: 3,
                    window_log: None,
                    checksum: Checksum::Xxh64,
                    feed,
                },
            ));
        }
        // Windows smaller than the input: matches stay inside them.
        if input.len() > 2000 {
            for window_log in [10, 11, 13, 16] {
                cases.push((
                    i,
                    Case {
                        name: name.clone(),
                        level: if window_log % 2 == 0 { 19 } else { 4 },
                        window_log: Some(window_log),
                        checksum: Checksum::Xxh64,
                        feed: if window_log == 13 {
                            Feed::Chunks(5000)
                        } else {
                            Feed::OneShot
                        },
                    },
                ));
            }
        }
    }
    cases
}

fn options(case: &Case) -> Options {
    let o = Options::new().level(case.level).checksum(case.checksum);
    match case.window_log {
        Some(w) => o.window_log(w),
        None => o,
    }
}

/// Every case's frame decodes to its input; the frames, counted, use every block type,
/// every literals section type, every table mode, every width of the sequence count,
/// both segment kinds and both checksum choices.
#[test]
fn every_frame_the_encoder_writes_decodes_to_its_input() {
    let mut inputs = corpus();
    inputs.extend(recorded_inputs());
    let cases = cases(&inputs);
    let frames = on_every_core(&cases, |(i, case)| {
        let input = &inputs[*i].1;
        let zst = write_frame(input, options(case), case.feed);
        let what = format!(
            "{} at level {} window {:?} {:?} {:?}",
            case.name, case.level, case.window_log, case.checksum, case.feed
        );
        let back = decode(&zst).unwrap_or_else(|e| panic!("{what}: {e}"));
        assert!(
            back == *input,
            "{what}: decoded {} of {} bytes, not the input",
            back.len(),
            input.len()
        );
        if let Some(w) = case.window_log
            && input.len() > 1 << w
        {
            assert_eq!(zst[4] & 0x20, 0, "{what}: a windowed frame");
            assert_eq!(10 + (zst[5] >> 3), w, "{what}: its window");
        }
        zst
    });
    let mut census = Census::default();
    for zst in &frames {
        census.add(zst);
    }
    assert_eq!(census.frames, cases.len());
    assert!(
        census.blocks.iter().all(|&n| n > 0),
        "raw, RLE, compressed blocks: {census:?}"
    );
    assert!(
        census.literals.iter().all(|&n| n > 0),
        "every literals type: {census:?}"
    );
    assert!(
        census.modes.iter().all(|&n| n > 0),
        "every table mode: {census:?}"
    );
    assert!(
        census.single_segment > 0 && census.single_segment < census.frames,
        "{census:?}"
    );
    assert!(
        census.with_checksum > 0 && census.with_checksum < census.frames,
        "{census:?}"
    );
    assert!(census.rle_extra_bits.iter().any(|&n| n > 0), "{census:?}");
    assert!(
        census.sequence_counts.iter().all(|&n| n > 0),
        "1-, 2- and 3-byte sequence counts: {census:?}"
    );
}

/// However the input is cut into writes, the frame is the same bytes: blocks fall at the
/// same places and the match finder sees the same history.
#[test]
fn the_frame_does_not_depend_on_how_writes_are_cut() {
    for (name, input) in [
        ("words", support::words(700_000, 21)),
        ("mixed", support::mixed(900_000, 22)),
    ] {
        for options in [
            Options::new().level(3).content_size(input.len() as u64),
            Options::new()
                .level(19)
                .window_log(17)
                .content_size(input.len() as u64),
        ] {
            let whole = encode::compress(&input, &options);
            for n in [1000, 65_536, 131_073, 400_000] {
                let mut enc = Encoder::new(Vec::new(), options);
                for piece in input.chunks(n) {
                    enc.write_all(piece).unwrap();
                }
                assert!(enc.finish().unwrap() == whole, "{name} in writes of {n}");
            }
        }
    }
}

/// A match 9 MiB back is found only in a window that reaches it: with a 16 MiB window
/// the repeat costs a few bytes, with libzstd -19's 8 MiB it costs its length again.
#[test]
fn a_window_reaches_as_far_as_it_declares() {
    let input = support::far_repeat(9);
    let near = encode::compress(&input, &Options::new().level(19));
    let far = encode::compress(&input, &Options::new().level(19).window_log(24));
    for zst in [&near, &far] {
        assert!(decode(zst).unwrap() == input);
    }
    assert!(far.len() < 16 * 1024 + 1024, "{} bytes", far.len());
    assert!(near.len() > 2 * 16 * 1024, "{} bytes", near.len());
    // The content fits 16 MiB: one segment, whose window is the content. Past 8 MiB it
    // does not, and the frame declares libzstd -19's window.
    assert_ne!(far[4] & 0x20, 0, "a single segment");
    assert_eq!(&far[5..9], &(input.len() as u32).to_le_bytes());
    assert_eq!(
        (near[4] & 0x20, near[5]),
        (0, (23 - 10) << 3),
        "an 8 MiB window"
    );
}

/// The content size declared up front is held: too few or too many bytes are refused,
/// and the frame written with the right count carries the size in its header.
#[test]
fn a_declared_content_size_is_held() {
    let input = support::words(300_000, 1);
    let options = Options::new().content_size(input.len() as u64);
    let mut enc = Encoder::new(Vec::new(), options);
    enc.write_all(&input[..1000]).unwrap();
    assert_eq!(
        enc.finish().unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    let mut enc = Encoder::new(Vec::new(), options);
    enc.write_all(&input).unwrap();
    assert_eq!(
        enc.write(b"x").unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    assert!(enc.write(b"").is_err(), "the encoder stays failed");
    let mut enc = Encoder::new(Vec::new(), options);
    for piece in input.chunks(10_000) {
        enc.write_all(piece).unwrap();
    }
    let zst = enc.finish().unwrap();
    // 300,000 bytes fit level 3's 2 MiB window: one segment, its size in 4 bytes.
    assert_eq!(
        zst[4] & 0xE0,
        2 << 6 | 0x20,
        "a single segment, a 4-byte size"
    );
    assert_eq!(&zst[5..9], &(input.len() as u32).to_le_bytes());
    assert!(decode(&zst).unwrap() == input);
    // Declared past the window, the size rides a windowed frame's header.
    let big = support::words(3 << 20, 2);
    let mut enc = Encoder::new(Vec::new(), Options::new().content_size(big.len() as u64));
    enc.write_all(&big).unwrap();
    let zst = enc.finish().unwrap();
    assert_eq!((zst[4] & 0xE0, zst[5]), (2 << 6, (21 - 10) << 3));
    assert_eq!(&zst[6..10], &(big.len() as u32).to_le_bytes());
    assert!(decode(&zst).unwrap() == big);
}

/// The negative control: our frames, damaged, are refused. A wrong checksum is named as
/// one, a frame cut short is an early end, and a flipped bit decodes to an error or to the
/// input itself, never to other bytes.
#[test]
fn a_damaged_frame_of_ours_is_refused() {
    let input = support::mixed(400_000, 31);
    let zst = encode::compress(&input, &Options::new().level(9));
    assert!(decode(&zst).unwrap() == input);

    let mut bad = zst.clone();
    let last = bad.len() - 1;
    bad[last] ^= 0x10;
    let e = decode(&bad).unwrap_err();
    assert!(
        matches!(
            e.get_ref().and_then(|e| e.downcast_ref::<DecodeError>()),
            Some(DecodeError::Checksum { .. })
        ),
        "{e}"
    );

    for cut in [5, zst.len() / 3, zst.len() - 5, zst.len() - 1] {
        let e = decode(&zst[..cut]).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof, "cut at {cut}");
    }

    let mut x = support::XorShift::new(0xBAD);
    let bits: Vec<usize> = (0..400).map(|_| x.below(zst.len() * 8)).collect();
    let refused = on_every_core(&bits, |&bit| {
        let mut bad = zst.clone();
        bad[bit / 8] ^= 1 << (bit % 8);
        match decode(&bad) {
            Ok(v) => {
                assert!(v == input, "bit {bit}: other bytes decoded as the content");
                false
            }
            Err(_) => true,
        }
    });
    assert!(
        refused.iter().filter(|&&r| r).count() > refused.len() * 9 / 10,
        "most flips are refused"
    );
}

/// After a flush, what the writer holds decodes to everything written so far and then
/// ends early (the frame is not finished). The control: before the flush, the last write
/// is still pending and not all of it decodes.
#[test]
fn a_flush_makes_everything_written_decodable() {
    let input = support::mixed(700_000, 51);
    for (level, window_log) in [(3, None), (19, Some(17))] {
        let mut options = Options::new().level(level);
        if let Some(w) = window_log {
            options = options.window_log(w);
        }
        let mut enc = Encoder::new(Vec::new(), options);
        let mut written = 0;
        for piece in input.chunks(90_001) {
            enc.write_all(piece).unwrap();
            written += piece.len();
            let (before, _) = partial(enc.get_ref());
            assert!(
                before.len() < written,
                "level {level}: {} of {written} bytes decode before the flush",
                before.len()
            );
            assert!(input.starts_with(&before), "level {level}: a prefix");
            enc.flush().unwrap();
            let (after, end) = partial(enc.get_ref());
            assert!(
                after == input[..written],
                "level {level}: {} of {written}",
                after.len()
            );
            assert_eq!(end, io::ErrorKind::UnexpectedEof, "level {level}");
        }
        assert!(
            decode(&enc.finish().unwrap()).unwrap() == input,
            "level {level}"
        );
    }
}

/// What a frame not yet finished decodes to, and how the decode ends.
fn partial(zst: &[u8]) -> (Vec<u8>, io::ErrorKind) {
    let mut out = Vec::new();
    let e = Decoder::with_buffer(zst)
        .read_to_end(&mut out)
        .expect_err("an unfinished frame ends early");
    (out, e.kind())
}
