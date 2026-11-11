// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::io::{self, Read};

use super::{DecodeError, Decoder};
use crate::frame::{self, Checksum};

/// A checksummed frame, a skippable frame, then a frame without a
/// checksum.
fn stream() -> Vec<u8> {
    let mut zst = Vec::new();
    frame::write_raw(&mut zst, b"checked", Checksum::Xxh64).unwrap();
    zst.extend(0x184D_2A50u32.to_le_bytes());
    zst.extend(2u32.to_le_bytes());
    zst.extend(b"sk");
    frame::write_raw(&mut zst, b"unchecked", Checksum::Omitted).unwrap();
    zst
}

/// The declared size is read from the header alone, and only a frame's header answers.
#[test]
fn the_declared_content_size_is_the_headers() {
    let mut zst = Vec::new();
    frame::write_raw(&mut zst, &[7u8; 70_000], Checksum::Xxh64).unwrap();
    assert_eq!(
        super::declared_content_size(&zst[..18]).unwrap(),
        Some(70_000)
    );
    let mut empty = Vec::new();
    frame::write_raw(&mut empty, b"", Checksum::Omitted).unwrap();
    assert_eq!(super::declared_content_size(&empty[..]).unwrap(), Some(0));
    // No Frame_Content_Size: FCS flag 0, not single-segment, a 1 KiB window.
    let none = [0x28, 0xb5, 0x2f, 0xfd, 0x00, 0x00];
    assert_eq!(super::declared_content_size(&none[..]).unwrap(), None);
    let skippable = stream()[zst_len(b"checked")..].to_vec();
    let e = super::declared_content_size(&skippable[..]).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::InvalidData);
    let e = super::declared_content_size(&zst[..5]).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof);
}

fn zst_len(content: &[u8]) -> usize {
    let mut zst = Vec::new();
    frame::write_raw(&mut zst, content, Checksum::Xxh64).unwrap();
    zst.len()
}

#[test]
fn require_checksum_refuses_at_the_first_frame_without_one() {
    let zst = stream();
    let mut all = Vec::new();
    Decoder::new(&zst[..]).read_to_end(&mut all).unwrap();
    assert_eq!(all, b"checkedunchecked");

    let mut decoder = Decoder::new(&zst[..]).require_checksum();
    let mut buf = [0u8; 64];
    let n = decoder.read(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"checked");
    let e = decoder.read(&mut buf).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::InvalidData);
    assert_eq!(
        e.get_ref().and_then(|e| e.downcast_ref::<DecodeError>()),
        Some(&DecodeError::NoChecksum)
    );
    assert!(decoder.read(&mut buf).is_err(), "the stream stays failed");
}

/// One frame decodes; a skippable frame, an empty second frame or a stray byte after the
/// first frame is refused before it is read, and only with `one_frame` set.
#[test]
fn one_frame_refuses_whatever_follows_or_precedes_the_frame() {
    let one = {
        let mut zst = Vec::new();
        frame::write_raw(&mut zst, b"exe", Checksum::Xxh64).unwrap();
        zst
    };
    let mut out = Vec::new();
    Decoder::new(&one[..])
        .one_frame()
        .read_to_end(&mut out)
        .unwrap();
    assert_eq!(out, b"exe");
    let skippable = [0x184D_2A50u32.to_le_bytes(), 2u32.to_le_bytes()].concat();
    let empty = {
        let mut zst = Vec::new();
        frame::write_raw(&mut zst, b"", Checksum::Xxh64).unwrap();
        zst
    };
    for (label, zst, before) in [
        (
            "an empty frame after",
            [&one[..], &empty].concat(),
            &b"exe"[..],
        ),
        (
            "a skippable frame after",
            [&one[..], &skippable, b"sk"].concat(),
            b"exe",
        ),
        ("a stray byte after", [&one[..], &[0]].concat(), b"exe"),
        (
            "a skippable frame before",
            [&skippable[..], b"sk", &one].concat(),
            b"",
        ),
        (
            "a skippable frame alone",
            [&skippable[..], b"sk"].concat(),
            b"",
        ),
    ] {
        let plain = Decoder::new(&zst[..]).read_to_end(&mut Vec::new());
        assert_ne!(
            plain
                .as_ref()
                .err()
                .and_then(|e| e.get_ref())
                .and_then(|e| e.downcast_ref::<DecodeError>()),
            Some(&DecodeError::NotOneFrame),
            "{label}: only one_frame refuses it"
        );
        let mut decoder = Decoder::new(&zst[..]).one_frame();
        let mut out = Vec::new();
        let e = decoder.read_to_end(&mut out).unwrap_err();
        assert_eq!(
            e.get_ref().and_then(|e| e.downcast_ref::<DecodeError>()),
            Some(&DecodeError::NotOneFrame),
            "{label}: {e}"
        );
        assert_eq!(out, before, "{label}");
        assert!(decoder.between_frames(), "{label}");
    }
}

#[test]
fn an_error_at_a_frames_magic_or_header_leaves_the_decoder_between_frames() {
    let mut whole = Vec::new();
    frame::write_raw(&mut whole, b"kept", Checksum::Xxh64).unwrap();
    let after = |tail: &[u8]| {
        let mut zst = whole.clone();
        zst.extend(tail);
        zst
    };
    let mut bad_sum = after(&whole);
    let at = bad_sum.len() - 1;
    bad_sum[at] ^= 1;
    for (zst, between, why) in [
        (after(b"junk"), true, "not a frame"),
        (after(b"ju"), true, "ends inside a frame"),
        (
            after(&[0x28, 0xb5, 0x2f, 0xfd, 0x01, 0x00, 7]),
            true,
            "dictionary",
        ),
        (
            after(&whole[..whole.len() - 3]),
            false,
            "ends inside a frame",
        ),
        (bad_sum, false, "checksum"),
    ] {
        let mut decoder = Decoder::new(&zst[..]);
        let mut out = Vec::new();
        let e = decoder.read_to_end(&mut out).unwrap_err();
        assert!(e.to_string().contains(why), "{e} names {why}");
        assert_eq!(decoder.between_frames(), between, "{why}");
        assert_eq!(out, b"kept", "{why}: the whole frame before it");
        assert!(decoder.read(&mut [0; 1]).is_err());
        assert_eq!(decoder.between_frames(), between, "a later read keeps it");
    }
}
