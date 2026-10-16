// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Frames the zstd 1.5.7 CLI and pzstd 1.5.7 wrote, kept as recorded answers
//! for the decoder: each `tests/recorded/cli/*.zst` file, the input it was
//! made from (regenerated here from `support`), and the SHA-256 and length
//! of what it decodes to.
//!
//! The set covers what the decoder must accept from real producers: the
//! packers' `zstd -19` from a pipe (no content size, checksum, an 8 MiB
//! window), `--ultra -22 --long=27` (a 128 MiB window declared), a negative
//! level without a checksum, a 1 KiB window (blocks smaller than 128 KiB),
//! a file input (a single-segment frame with its content size), pzstd's
//! skippable-framed multi-frame output, and the empty frame; and two shapes
//! the ZC.4 level sweep reached only while libzstd was its encoder: RLE
//! sequence tables whose codes carry extra bits, and a match 9 MiB back,
//! past any window but a long one (the recorded sweep, sweep.rs, has the
//! first; this file alone has the second).
//!
//! Recorded 2026-10-15 (docs/AUDIT-third-party-2026-10-15.md ZC.4) with
//! Homebrew's zstd/pzstd v1.5.7. A scratch crate outside the workspace wrote
//! each input with `support::{words, mixed, repetitive, one_code,
//! far_repeat}`, then:
//!
//! ```text
//! zstd -q -19 -c < words-96k.bin > words-96k.19.zst
//! zstd -q --ultra -22 --long=27 -c < words-96k.bin > words-96k.ultra22-long27.zst
//! zstd -q --fast=5 --no-check -c < words-32k.bin > words-32k.fast5-nocheck.zst
//! zstd -q -19 --zstd=wlog=10 -c < words-48k.bin > words-48k.19-wlog10.zst
//! zstd -q -19 -f mixed-300k.bin -o mixed-300k.19-file.zst
//! pzstd -q -p 2 -1 -f repetitive-2700k.bin -o repetitive-2700k.pzstd1.zst
//! zstd -q -c < /dev/null > empty.zst
//! zstd -q --ultra -22 -c < one-code.bin > one-code.ultra22.zst
//! zstd -q --ultra -22 --long=27 -c < far-repeat-9m.bin > far-repeat-9m.ultra22-long27.zst
//! ```
//!
//! and `zstd -dc <f> | shasum -a 256` gave the digests below, equal to each
//! input's own. libzstd decoded every file to its input, and refused each
//! one byte short, here until ZC.6 took it out of the graph (13f0e82be);
//! these files outlive it.
//!
//! [`aterm_zstd::Decoder`] is held to the same rows: every file decodes to
//! its input, read whole or a few bytes at a time, alone or all back to back
//! with a skippable frame between, and every cut of a file inside a frame is
//! refused.

mod support;

use std::io::{self, Read};

use aterm_digest::Sha256;
use aterm_zstd::frame::MAGIC;
use aterm_zstd::{DecodeError, Decoder};

/// One recorded CLI frame and what it decodes to.
struct Recorded {
    file: &'static str,
    /// The input, regenerated.
    input: fn() -> Vec<u8>,
    len: usize,
    sha256: &'static str,
}

const RECORDED: &[Recorded] = &[
    Recorded {
        file: "words-96k.19.zst",
        input: || support::words(96 * 1024, 1),
        len: 98_304,
        sha256: "9805e2b6bd20fa545aee133c146121f8942afcf0500511d258669e5a47202d4f",
    },
    Recorded {
        file: "words-96k.ultra22-long27.zst",
        input: || support::words(96 * 1024, 1),
        len: 98_304,
        sha256: "9805e2b6bd20fa545aee133c146121f8942afcf0500511d258669e5a47202d4f",
    },
    Recorded {
        file: "words-32k.fast5-nocheck.zst",
        input: || support::words(32 * 1024, 1),
        len: 32_768,
        sha256: "68396c14698331a253294cb0b904efc52118d1d536e0b607bdfc5b94ffdb1333",
    },
    Recorded {
        file: "words-48k.19-wlog10.zst",
        input: || support::words(48 * 1024, 1),
        len: 49_152,
        sha256: "995d17b08f521c5fc61651c8c6c11f791560052dc92eedb461af8d5d3373bb31",
    },
    Recorded {
        file: "mixed-300k.19-file.zst",
        input: || support::mixed(300 * 1024, 2),
        len: 307_200,
        sha256: "5cedfdc01f0bc886150e6c17394c2fa285f948c7fcb3c08a9e62808fd3ea662c",
    },
    Recorded {
        file: "repetitive-2700k.pzstd1.zst",
        input: || support::repetitive(2_700_000, 1),
        len: 2_700_000,
        sha256: "19ef76581f253e239cdba41a45f3760d06061e66b33e18c5a9789540e2084438",
    },
    Recorded {
        file: "one-code.ultra22.zst",
        input: || support::one_code(24, 1),
        len: 132_416,
        sha256: "ca74d51785df118c94062260167ff1845056794a429806497f95e3caeafe715a",
    },
    Recorded {
        file: "far-repeat-9m.ultra22-long27.zst",
        input: || support::far_repeat(9),
        len: 9_469_952,
        sha256: "d30fa1026f2d8a7157630edf99f867f5346aebdbc2d10520123a373c83441041",
    },
    Recorded {
        file: "empty.zst",
        input: Vec::new,
        len: 0,
        sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    },
];

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn frame(file: &str) -> Vec<u8> {
    let path = format!("{}/tests/recorded/cli/{file}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// The recording holds: each input regenerates to the recorded digest, so
/// the decoder's answer below is held to the CLI's input, not just to a
/// recorded hash.
#[test]
fn every_recorded_input_regenerates_to_its_digest() {
    for r in RECORDED {
        let input = (r.input)();
        assert_eq!(
            (input.len(), sha256_hex(&input).as_str()),
            (r.len, r.sha256),
            "{} input",
            r.file
        );
    }
}

/// The two shapes the set was extended for are in it, so decoding them is
/// not vacuous. `one-code`'s second block declares 24 sequences (byte 24)
/// with all three tables RLE (modes byte `0x54`), their codes literal length
/// 16, offset 7 and match length 34, each with extra bits. `far-repeat`
/// declares a 2^27 window (descriptor `0x88`) and holds its 16 KiB of noise
/// twice in less than twice its size: the second copy is a match 9 MiB back.
#[test]
fn the_recorded_set_holds_rle_codes_with_extra_bits_and_a_far_match() {
    let one_code = frame("one-code.ultra22.zst");
    assert!(
        one_code.windows(5).any(|w| w == [24, 0x54, 16, 7, 34]),
        "no all-RLE sequences section with codes 16, 7, 34"
    );
    let far = frame("far-repeat-9m.ultra22-long27.zst");
    assert_eq!(far[5], 0x88, "window descriptor");
    assert!(far.len() < 2 * 16 * 1024, "{} bytes", far.len());
}

fn aterm(zst: &[u8]) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    Decoder::with_buffer(zst).read_to_end(&mut out)?;
    Ok(out)
}

/// Reads through `Decoder::new` (its own `BufReader`) `step` bytes at a time.
fn aterm_in_steps(zst: &[u8], step: usize) -> io::Result<Vec<u8>> {
    let mut decoder = Decoder::new(zst);
    let mut out = Vec::new();
    let mut buf = vec![0u8; step];
    loop {
        match decoder.read(&mut buf)? {
            0 => return Ok(out),
            n => out.extend_from_slice(&buf[..n]),
        }
    }
}

#[test]
fn the_decoder_decodes_every_recorded_frame_to_its_input() {
    for r in RECORDED {
        let zst = frame(r.file);
        let decoded = aterm(&zst).unwrap_or_else(|e| panic!("{}: {e}", r.file));
        assert_eq!(
            (decoded.len(), sha256_hex(&decoded).as_str()),
            (r.len, r.sha256),
            "{}",
            r.file
        );
        for step in [1, 7, 4096] {
            if r.len > 100_000 && step < 4096 {
                continue;
            }
            let stepped = aterm_in_steps(&zst, step).unwrap_or_else(|e| panic!("{}: {e}", r.file));
            assert!(stepped == decoded, "{} in steps of {step}", r.file);
        }
    }
}

/// [`Decoder::require_checksum`] refuses the one recorded frame written
/// without a checksum (`--no-check`: bit 2 of its descriptor clear), at its
/// header and before any of its content, and reads every other file as the
/// plain decoder does. The plain decoder reading that frame (above) is the
/// negative control.
#[test]
fn require_checksum_refuses_only_the_frame_written_without_one() {
    for r in RECORDED {
        let zst = frame(r.file);
        // The first zstd frame's descriptor; pzstd writes a 12-byte
        // skippable frame first.
        let first = if zst[..4] == MAGIC.to_le_bytes() {
            0
        } else {
            12
        };
        let has_checksum = zst[first + 4] & 0x04 != 0;
        assert_eq!(
            has_checksum,
            r.file != "words-32k.fast5-nocheck.zst",
            "{}",
            r.file
        );
        let mut out = Vec::new();
        let read = Decoder::new(&zst[..])
            .require_checksum()
            .read_to_end(&mut out);
        if has_checksum {
            read.unwrap_or_else(|e| panic!("{}: {e}", r.file));
            assert_eq!(sha256_hex(&out), r.sha256, "{}", r.file);
        } else {
            let e = read.expect_err(r.file);
            assert_eq!(e.kind(), io::ErrorKind::InvalidData, "{}", r.file);
            assert_eq!(
                e.get_ref().and_then(|e| e.downcast_ref::<DecodeError>()),
                Some(&DecodeError::NoChecksum),
                "{}",
                r.file
            );
            assert!(
                out.is_empty(),
                "{}: content read before the refusal",
                r.file
            );
        }
    }
}

/// Every recorded file back to back, a skippable frame before each, is one
/// stream: the inputs, concatenated.
#[test]
fn the_decoder_reads_the_recorded_frames_as_one_stream() {
    let mut stream = Vec::new();
    let mut want = Vec::new();
    for (i, r) in RECORDED.iter().enumerate() {
        stream.extend((0x184D_2A50u32 + i as u32).to_le_bytes());
        stream.extend(3u32.to_le_bytes());
        stream.extend(b"pad");
        stream.extend(frame(r.file));
        want.extend((r.input)());
    }
    assert!(aterm(&stream).expect("one stream") == want);
}

/// What each zstd frame of a multi-frame file holds, in order: pzstd split
/// its 2,700,000-byte input at 2 MiB (`zstd -dc` of the file cut after its
/// first frame gave 2,097,152 bytes; zstd 1.5.7, 2026-10-16, ZC.6). Every
/// other file is one frame holding all of its input.
const FRAME_CONTENT: &[(&str, &[usize])] =
    &[("repetitive-2700k.pzstd1.zst", &[2_097_152, 602_848])];

/// Cut anywhere inside a frame, a file is an early end: never a panic, never
/// a partial success. A cut at a frame boundary past the first (after
/// pzstd's skippable frame, or between its frames) is a shorter stream: it
/// decodes to exactly the content of the frames before the cut
/// ([`FRAME_CONTENT`]; libzstd agreed at every cut while it was the
/// oracle). Cuts: the first and last 64 bytes, 64 spread through the rest,
/// and every frame boundary.
#[test]
fn the_decoder_refuses_every_recorded_frame_cut_short() {
    for r in RECORDED {
        let zst = frame(r.file);
        let len = zst.len();
        let whole = aterm(&zst).unwrap_or_else(|e| panic!("{}: {e}", r.file));
        let sizes = FRAME_CONTENT
            .iter()
            .find(|f| f.0 == r.file)
            .map_or_else(|| vec![r.len], |f| f.1.to_vec());
        assert_eq!(sizes.iter().sum::<usize>(), r.len, "{}", r.file);
        // Each boundary, and the content of every zstd frame before it.
        let mut held = 0;
        let mut sizes = sizes.into_iter();
        let ends: Vec<(usize, usize)> = frame_ends(&zst)
            .into_iter()
            .map(|(end, zstd)| {
                if zstd {
                    held += sizes
                        .next()
                        .unwrap_or_else(|| panic!("{}: a frame too many", r.file));
                }
                (end, held)
            })
            .collect();
        assert!(sizes.next().is_none(), "{}: a frame too few", r.file);
        let mut cuts: Vec<usize> = (0..len.min(64))
            .chain((0..len).step_by(len.div_ceil(64).max(1)))
            .chain(len.saturating_sub(64)..len)
            .chain(ends.iter().map(|e| e.0).filter(|&e| e < len))
            .collect();
        cuts.sort_unstable();
        cuts.dedup();
        for cut in cuts {
            let what = format!("{} cut at {cut} of {len}", r.file);
            let boundary = ends.iter().find(|e| e.0 == cut);
            match aterm(&zst[..cut]) {
                Ok(v) => {
                    let &(_, held) =
                        boundary.unwrap_or_else(|| panic!("{what}: decoded inside a frame"));
                    assert!(v == whole[..held], "{what}: not the frames before the cut");
                }
                Err(e) => {
                    assert!(cut == 0 || boundary.is_none(), "{what}: {e}");
                    assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof, "{what}: {e}");
                }
            }
        }
    }
}

/// Where each frame of `zst` ends (skippable frames included), walked from
/// the headers alone, and whether it is a zstd frame (one that holds
/// content): the cuts at which a stream is whole.
fn frame_ends(zst: &[u8]) -> Vec<(usize, bool)> {
    let le = |b: &[u8]| {
        b.iter()
            .rev()
            .fold(0usize, |acc, &x| acc << 8 | usize::from(x))
    };
    let mut ends = Vec::new();
    let mut at = 0;
    while at < zst.len() {
        if zst[at..at + 4] != MAGIC.to_le_bytes() {
            at += 8 + le(&zst[at + 4..at + 8]);
            ends.push((at, false));
            continue;
        }
        let fhd = zst[at + 4];
        let single = fhd & 0x20 != 0;
        let mut pos = at + 5 + usize::from(!single) + [0, 1, 2, 4][usize::from(fhd & 3)];
        pos += [usize::from(single), 2, 4, 8][usize::from(fhd >> 6)];
        loop {
            let header = le(&zst[pos..pos + 3]);
            pos += 3 + if (header >> 1) & 3 == 1 {
                1
            } else {
                header >> 3
            };
            if header & 1 == 1 {
                break;
            }
        }
        at = pos + if fhd & 4 != 0 { 4 } else { 0 };
        ends.push((at, true));
    }
    ends
}
