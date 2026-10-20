// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! [`aterm_zstd::Decoder`] on frames built here, byte by byte or by
//! [`frame::write_raw`]: what the recorded CLI frames (cli_decode.rs,
//! sweep.rs) cannot hold, because no encoder writes it.
//!
//! * every raw frame, at each Frame_Content_Size width and block boundary,
//!   decodes to exactly its content, alone and back to back;
//! * damage is refused with the rule it breaks, and the checksum is what
//!   catches a flipped content byte (without it the same flip decodes, to
//!   the wrong bytes: the negative control);
//! * hand-made headers past a limit (window, dictionary, reserved bit,
//!   content size) and a match past the declared window are refused.
//!
//! Until docs/AUDIT-third-party-2026-10-15.md ZC.6 took libzstd out of the
//! graph, libzstd 1.5.7 decoded the same raw frames and refused the same
//! damaged and hand-made ones beside this decoder (tests/libzstd.rs and
//! tests/decode_libzstd.rs at 13f0e82be); it accepted the two valid
//! window-limit frames below with the same bytes.

use std::io::{self, Read};

use crate::support::noise;
use aterm_zstd::frame::{self, BLOCK_MAX, Checksum, MAGIC, WINDOW_MAX};
use aterm_zstd::{DecodeError, Decoder};

fn ours(zst: &[u8]) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    Decoder::with_buffer(zst).read_to_end(&mut out)?;
    Ok(out)
}

/// The refusal inside an `InvalidData` error.
fn refusal(e: &io::Error) -> Option<&DecodeError> {
    e.get_ref()
        .and_then(|inner| inner.downcast_ref::<DecodeError>())
}

fn raw(content: &[u8], checksum: Checksum) -> Vec<u8> {
    let mut out = Vec::new();
    frame::write_raw(&mut out, content, checksum).expect("write_raw");
    out
}

/// Lengths at every Frame_Content_Size width edge and block boundary.
const LENGTHS: &[usize] = &[
    0,
    1,
    3,
    4,
    31,
    32,
    33,
    255,
    256,
    257,
    1000,
    65_535,
    65_791,
    65_792,
    65_793,
    BLOCK_MAX - 1,
    BLOCK_MAX,
    BLOCK_MAX + 1,
    2 * BLOCK_MAX,
    2 * BLOCK_MAX + 1,
    1_000_003,
];

#[test]
fn every_raw_frame_decodes_to_its_content() {
    for &len in LENGTHS {
        let content = noise(len, 0x5EED ^ len as u64);
        for checksum in [Checksum::Xxh64, Checksum::Omitted] {
            let f = raw(&content, checksum);
            let what = format!("len {len} {checksum:?}");
            assert!(ours(&f).expect(&what) == content, "{what}");
            // Read through `Decoder::new`'s own buffer, 4 KiB at a time.
            let mut stepped = Vec::new();
            let mut decoder = Decoder::new(&f[..]);
            let mut buf = vec![0u8; 4096];
            loop {
                match decoder.read(&mut buf).expect(&what) {
                    0 => break,
                    n => stepped.extend_from_slice(&buf[..n]),
                }
            }
            assert!(stepped == content, "{what} in 4 KiB reads");
        }
    }
}

/// Two raw frames back to back are one stream.
#[test]
fn concatenated_raw_frames_decode_as_one_stream() {
    let a = noise(BLOCK_MAX + 5, 1);
    let b = noise(300, 2);
    let mut stream = raw(&a, Checksum::Xxh64);
    stream.extend(raw(&b, Checksum::Omitted));
    let mut want = a;
    want.extend(b);
    assert!(ours(&stream).expect("two frames") == want);
}

/// Damage refused, each with the rule it breaks, and the proof that the
/// checksum is what catches a flipped content byte: without it the same flip
/// decodes, to the wrong bytes.
#[test]
fn damaged_raw_frames_are_refused() {
    for &len in &[1, 300, BLOCK_MAX + 1] {
        let content = noise(len, 3);
        let good = raw(&content, Checksum::Xxh64);
        // The content's last byte sits just before the checksum.
        let last_content = good.len() - 5;
        assert_eq!(good[last_content], content[len - 1]);

        let mut flipped = good.clone();
        flipped[last_content] ^= 0x80;
        let e = ours(&flipped).expect_err("content flip");
        assert!(
            matches!(refusal(&e), Some(DecodeError::Checksum { .. })),
            "content flip, len {len}: {e}"
        );

        let mut bad_sum = good.clone();
        let end = bad_sum.len() - 1;
        bad_sum[end] ^= 0x01;
        let e = ours(&bad_sum).expect_err("checksum flip");
        assert!(
            matches!(refusal(&e), Some(DecodeError::Checksum { .. })),
            "checksum flip, len {len}: {e}"
        );

        let e = ours(&good[..good.len() - 1]).expect_err("truncated");
        assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof, "len {len}");

        let mut bad_magic = good.clone();
        bad_magic[0] ^= 0x01;
        let e = ours(&bad_magic).expect_err("magic");
        assert!(
            matches!(refusal(&e), Some(DecodeError::BadMagic(_))),
            "len {len}: {e}"
        );

        // No checksum: the same flip decodes, to the wrong bytes.
        let mut unchecked = raw(&content, Checksum::Omitted);
        let last = unchecked.len() - 1;
        unchecked[last] ^= 0x80;
        let decoded = ours(&unchecked).expect("no checksum to fail");
        assert_eq!(decoded.len(), len);
        assert_ne!(decoded, content, "len {len}");
    }
}

/// Why `write_raw` stops at [`WINDOW_MAX`]: a single-segment frame's window
/// is its content size, and a decoder refuses one past the cap before it
/// reads a block. The control declares exactly `WINDOW_MAX` over the same
/// one-byte body: refused too, for the body (the size it does not fill),
/// not the window. (libzstd's own cap is one byte higher, 2^27 + 1; the
/// Decoder docs list it among the places this decoder is stricter.)
#[test]
fn a_single_segment_window_past_the_cap_is_refused() {
    let frame = |declared: usize| {
        let mut f = MAGIC.to_le_bytes().to_vec();
        f.push(2 << 6 | 1 << 5); // a 4-byte Frame_Content_Size, Single_Segment
        f.extend(u32::try_from(declared).expect("fits").to_le_bytes());
        f.extend([1 << 3 | 1, 0x00, 0x00, b'A']); // one 1-byte last raw block
        f
    };
    let past = ours(&frame(WINDOW_MAX + 1)).expect_err("past the cap");
    assert_eq!(
        refusal(&past),
        Some(&DecodeError::WindowTooLarge(WINDOW_MAX as u64 + 1)),
        "{past}"
    );
    let at = ours(&frame(WINDOW_MAX)).expect_err("1 byte, not 2^27");
    assert_eq!(
        refusal(&at),
        Some(&DecodeError::ContentSize {
            declared: WINDOW_MAX as u64,
            decoded: 1
        }),
        "{at}"
    );
}

/// Hand-made headers the decoder refuses: a window past 2^27, a dictionary,
/// the reserved bit, a content size the blocks do not fill or overrun.
#[test]
fn hand_made_headers_are_refused() {
    let header = |descriptor: u8, rest: &[u8]| {
        let mut f = MAGIC.to_le_bytes().to_vec();
        f.push(descriptor);
        f.extend(rest);
        // One empty last raw block.
        f.extend([1, 0, 0]);
        f
    };
    let cases: [(&str, Vec<u8>, DecodeError); 5] = [
        (
            "window log 28",
            header(0x00, &[18 << 3]),
            DecodeError::WindowTooLarge(1 << 28),
        ),
        (
            "dictionary 9",
            header(0x01, &[0x00, 9]),
            DecodeError::Dictionary(9),
        ),
        (
            "reserved bit",
            header(0x08, &[0x00]),
            DecodeError::ReservedBit,
        ),
        (
            "content size 1, empty block",
            header(0x20, &[1]),
            DecodeError::ContentSize {
                declared: 1,
                decoded: 0,
            },
        ),
        (
            "bad magic",
            vec![0x28, 0xB5, 0x2F, 0xFE, 0, 0],
            DecodeError::BadMagic(0xFE2F_B528),
        ),
    ];
    for (what, zst, want) in cases {
        let e = ours(&zst).unwrap_err();
        assert_eq!(refusal(&e), Some(&want), "{what}: {e}");
    }
    // Overrun: a raw block of 2 bytes in a frame that declares 1.
    let mut f = MAGIC.to_le_bytes().to_vec();
    f.extend([0x20, 1, 2 << 3 | 1, 0, 0, b'a', b'b']);
    let e = ours(&f).unwrap_err();
    assert!(matches!(refusal(&e), Some(DecodeError::Corrupt(_))), "{e}");
    // Window log 27 is accepted.
    assert!(ours(&header(0x00, &[17 << 3])).is_ok());
}

/// A literal prefix cannot extend the declared match distance. History
/// still holds the byte 1025 back after one literal of the next block,
/// but a 1024-byte window does not authorize that match. The two valid
/// frames are the control (libzstd decoded both to the same bytes).
#[test]
fn a_literal_prefix_does_not_extend_the_declared_window() {
    let history: Vec<u8> = (0..1024).map(|i| (i % 251) as u8).collect();
    let frame = |window_descriptor: u8, offset_extra: u8| {
        let mut zst = MAGIC.to_le_bytes().to_vec();
        zst.extend([0, window_descriptor]); // no FCS/checksum, explicit window
        zst.extend([0, 0x20, 0]); // nonlast raw block, 1024 bytes
        zst.extend_from_slice(&history);
        zst.extend([9 << 3 | 5, 0, 0]); // last compressed block, 9 bytes
        zst.extend([
            8,
            b'B', // one raw literal
            1,
            0x54, // one sequence, all three tables RLE
            1,
            10,
            0, // LL=1, OF base=1024 with 10 extra bits, ML=3
            offset_extra,
            4, // extra bits, then the end marker at bit 10
        ]);
        zst
    };

    let mut want = history.clone();
    want.extend([b'B', 0, 1, 2]);
    let wider = frame(8, 4); // 2048-byte window, resolved offset 1025
    assert_eq!(ours(&wider).unwrap(), want, "the sequence is valid");

    let mut at_limit = history.clone();
    at_limit.extend([b'B', 1, 2, 3]);
    let exact = frame(0, 3); // 1024-byte window, resolved offset 1024
    assert_eq!(ours(&exact).unwrap(), at_limit, "the exact limit is valid");

    let too_far = frame(0, 4); // same sequence, window now one byte too short
    let mut decoder = Decoder::with_buffer(&too_far[..]);
    let mut out = Vec::new();
    let err = decoder.read_to_end(&mut out).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert!(matches!(refusal(&err), Some(DecodeError::Corrupt(_))));
    assert_eq!(out, history, "the invalid block releases no bytes");
    assert!(
        decoder.read(&mut [0u8; 1]).is_err(),
        "the error stays final"
    );
}
