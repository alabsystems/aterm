// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Hand-built streams for what no recorded stream carries — metadata meta-blocks, the LSB6
//! and MSB6 context modes, a copy that runs past its meta-block — and the format's other
//! refusals, each against its passing twin.

use std::io::{self, Read};

use aterm_brotli::DecodeError;

use crate::support::{BitWriter, Trickle, decode};

fn refusal(br: &[u8]) -> DecodeError {
    let err = decode(br).unwrap_err();
    err.get_ref()
        .and_then(|e| e.downcast_ref::<DecodeError>())
        .cloned()
        .unwrap_or_else(|| panic!("not a stream refusal: {err}"))
}

/// The literal codes of [`context_stream`]: tree 0 codes `0x05` as `0` and `0x40` as `1`,
/// tree 1 `0x7e` and `0xc3`.
const TREES: [[u8; 2]; 2] = [[0x05, 0x40], [0x7e, 0xc3]];

/// One compressed meta-block of `picks.len()` literals in one insert, under context mode
/// `mode` (0 LSB6, 1 MSB6), whose context map sends an even context to literal tree 0 and
/// an odd one to tree 1; `picks` are the literals' code bits.
fn context_stream(mode: u64, picks: &[u64]) -> Vec<u8> {
    let mut w = BitWriter::default();
    w.bits(0, 1) // WBITS 16
        .bits(1, 1)
        .bits(0, 1) // ISLAST, not empty
        .bits(0, 2)
        .bits(picks.len() as u64 - 1, 16) // MLEN in four nibbles
        .bits(0, 1)
        .bits(0, 1)
        .bits(0, 1) // one block type of each category
        .bits(0, 2)
        .bits(0, 4) // NPOSTFIX 0, NDIRECT 0
        .bits(mode, 2)
        .bits(1, 1)
        .bits(0, 3) // NTREESL 2
        .bits(0, 1) // no run-length codes in the context map
        .simple2(0, 1, 2);
    for context in 0..64 {
        w.bits(context & 1, 1);
    }
    w.bits(0, 1) // no inverse move-to-front
        .bits(0, 1) // NTREESD 1
        .simple2(u64::from(TREES[0][0]), u64::from(TREES[0][1]), 256)
        .simple2(u64::from(TREES[1][0]), u64::from(TREES[1][1]), 256);
    // Symbol 48 is cell 0 (insert codes 0..7, copy codes 0..7, the implicit distance)
    // with insert code 6 (an insert of 6 plus one extra bit) and copy code 0.
    w.simple1(48, 704) // the one command code
        .simple1(0, 64); // the one distance code
    assert_eq!(picks.len(), 7, "insert code 6 with its extra bit set");
    w.bits(1, 1); // insert 7
    for &p in picks {
        w.bits(p, 1);
    }
    w.finish()
}

/// What RFC 7932 §7.1 says [`context_stream`] decodes to: LSB6's context is `p1 & 0x3f`,
/// MSB6's `p1 >> 2`, from a `p1` of 0 at the stream's start.
fn context_model(mode: u64, picks: &[u64]) -> Vec<u8> {
    let mut p1 = 0u8;
    let mut out = Vec::new();
    for &p in picks {
        let context = if mode == 0 { p1 & 0x3f } else { p1 >> 2 };
        p1 = TREES[usize::from(context & 1)][p as usize];
        out.push(p1);
    }
    out
}

/// THE LSB6 AND MSB6 CONTEXT MODES, which no stream the CLI writes uses: the same bits
/// under each mode pick their literals through the context RFC 7932 defines for it.
#[test]
fn the_lsb6_and_msb6_modes_select_literal_codes_by_their_own_context() {
    let picks = [1, 0, 1, 1, 0, 0, 1];
    let (lsb6, msb6) = (context_model(0, &picks), context_model(1, &picks));
    assert_ne!(lsb6, msb6, "the stream tells the two modes apart");
    assert_eq!(decode(&context_stream(0, &picks)).unwrap(), lsb6);
    assert_eq!(decode(&context_stream(1, &picks)).unwrap(), msb6);
}

/// Metadata meta-blocks (§9.2), which no encoder writes: skipped between data, empty, and
/// as the last meta-block; and each of their rules refused against that stream.
#[test]
fn metadata_blocks_are_skipped_and_their_rules_hold() {
    let stream = |reserved: u64, last_skip: &[u64], pad: Option<u64>| {
        let mut w = BitWriter::default();
        w.bits(0, 1) // WBITS 16
            .bits(0, 1)
            .bits(3, 2)
            .bits(reserved, 1)
            .bits(1, 2)
            .bits(4, 8); // five bytes of metadata
        match pad {
            Some(bit) => w.bits(bit, 1),
            None => w.pad(),
        };
        w.bytes(b"meta!")
            .bits(0, 1)
            .bits(0, 2)
            .bits(1, 16)
            .bits(1, 1)
            .pad()
            .bytes(b"hi") // an uncompressed "hi"
            .bits(0, 1)
            .bits(3, 2)
            .bits(0, 1)
            .bits(0, 2)
            .pad() // metadata of no bytes
            .bits(1, 1)
            .bits(0, 1)
            .bits(3, 2)
            .bits(0, 1)
            .bits(last_skip.len() as u64, 2);
        for &b in last_skip {
            w.bits(b, 8);
        }
        let n = last_skip
            .iter()
            .enumerate()
            .fold(0, |n, (i, &b)| n | (b as usize) << (8 * i))
            + 1;
        w.pad().bytes(&vec![b'z'; n]).finish()
    };
    let good = stream(0, &[1], None);
    assert_eq!(decode(&good).unwrap(), b"hi");
    assert_eq!(decode(&stream(0, &[1, 1], None)).unwrap(), b"hi");
    assert_eq!(
        refusal(&stream(1, &[1], None)),
        DecodeError::Corrupt("a metadata block sets its reserved bit")
    );
    assert_eq!(
        refusal(&stream(0, &[1, 0], None)),
        DecodeError::Corrupt("a metadata length's last byte is zero")
    );
    assert_eq!(
        refusal(&stream(0, &[1], Some(1))),
        DecodeError::Corrupt("a metadata block's padding is not zero")
    );
    assert_eq!(decode(&stream(0, &[1], Some(0))).unwrap(), b"hi");
    assert_eq!(refusal(&good[..good.len() - 1]), DecodeError::Truncated);
}

/// A meta-block of `mlen` bytes: the literals `ab`, then a copy of 4 from distance 1 (the
/// one direct distance code, `NDIRECT` 1).
fn copy_stream(mlen: u64) -> Vec<u8> {
    let mut w = BitWriter::default();
    w.bits(0, 1)
        .bits(1, 1)
        .bits(0, 1)
        .bits(0, 2)
        .bits(mlen - 1, 16)
        .bits(0, 3) // one block type of each category
        .bits(0, 2)
        .bits(1, 4) // NPOSTFIX 0, NDIRECT 1
        .bits(0, 2) // LSB6
        .bits(0, 1)
        .bits(0, 1) // NTREESL 1, NTREESD 1
        .simple2(u64::from(b'a'), u64::from(b'b'), 256)
        // Cell 2 (insert codes 0..7, copy codes 0..7, explicit distance): insert code 2
        // (2 literals) and copy code 2 (a copy of 4) are symbol 128 + (2 << 3) + 2.
        .simple1(146, 704)
        .simple1(16, 65)
        .bits(0, 1)
        .bits(1, 1);
    w.finish()
}

/// A COPY THAT RUNS PAST ITS META-BLOCK is refused, as the C decoder refuses it, where the
/// same command in a meta-block long enough for it decodes.
#[test]
fn a_copy_past_its_meta_block_is_refused() {
    assert_eq!(decode(&copy_stream(6)).unwrap(), b"abbbbb");
    assert_eq!(
        refusal(&copy_stream(5)),
        DecodeError::Corrupt("a copy runs past its meta-block")
    );
}

/// The stream's own framing rules: the padding after the last meta-block is zero, nothing
/// follows it, and a length's top nibble past the fourth is not zero.
#[test]
fn the_framing_rules_refuse_what_they_name() {
    assert_eq!(decode(&[0b110]).unwrap(), b"", "the empty stream");
    assert_eq!(
        refusal(&[0b1110]),
        DecodeError::Corrupt("the padding after the last meta-block is not zero")
    );
    assert_eq!(refusal(&[0b110, 0]), DecodeError::TrailingBytes);
    assert_eq!(refusal(&[]), DecodeError::Truncated);
    let length = |nibbles: u64, mlen_minus_1: u64| {
        let mut w = BitWriter::default();
        w.bits(0, 1)
            .bits(0, 1)
            .bits(nibbles - 4, 2)
            .bits(mlen_minus_1, 4 * nibbles as u32)
            .bits(1, 1)
            .pad()
            .bytes(&vec![b'x'; mlen_minus_1 as usize + 1])
            .bits(1, 1)
            .bits(1, 1);
        w.finish()
    };
    assert_eq!(decode(&length(4, 1)).unwrap(), b"xx");
    assert_eq!(decode(&length(5, 0x1_0000)).unwrap().len(), 0x1_0001);
    assert_eq!(
        refusal(&length(5, 1)),
        DecodeError::Corrupt("a meta-block length's last nibble is zero")
    );
}

/// After an error every later read fails: nothing past a refusal is handed out.
#[test]
fn an_error_ends_the_stream_for_good() {
    let mut br = Vec::new();
    aterm_brotli::write_uncompressed(&mut br, &[7u8; 300_000]).unwrap();
    br.push(0);
    let mut decoder = aterm_brotli::Decoder::new(&br[..]);
    let mut out = Vec::new();
    assert!(decoder.read_to_end(&mut out).is_err());
    assert_eq!(
        out.len(),
        300_000,
        "everything before the trailing byte was handed out"
    );
    let mut buf = [0u8; 16];
    assert!(decoder.read(&mut buf).is_err());
    assert!(decoder.read(&mut buf).is_err());
    // The refused copy's own step had already written the literals `ab`: they stay unread.
    let br = copy_stream(5);
    let mut decoder = aterm_brotli::Decoder::new(&br[..]);
    assert!(decoder.read(&mut buf).is_err());
    assert_eq!(
        decoder.decoded(),
        0,
        "bytes written inside a failed step are rolled back from decoded()"
    );
    assert!(
        decoder.read(&mut buf).is_err(),
        "nothing the failed step wrote"
    );
}

/// The writer's streams are read back whole however the input arrives, and a reader's
/// own error passes through as it is.
#[test]
fn written_streams_read_back_through_any_reader() {
    let content: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
    let mut br = Vec::new();
    aterm_brotli::write_uncompressed(&mut br, &content).unwrap();
    for step in [1, 2, 3, 7, 4096, usize::MAX] {
        let mut out = Vec::new();
        aterm_brotli::Decoder::new(Trickle { rest: &br, step })
            .read_to_end(&mut out)
            .unwrap();
        assert_eq!(out, content, "{step}");
    }
    struct Failing;
    impl Read for Failing {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the reader's own",
            ))
        }
    }
    let err = aterm_brotli::Decoder::new(Failing)
        .read(&mut [0u8; 8])
        .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
}
