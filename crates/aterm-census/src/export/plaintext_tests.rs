// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

/// What the zstd CLI 1.5.7 (`zstd -19`) wrote for `"zebra fish " * 40 +
/// "zebrafish\n"`: the second word is two matches, so no byte run spells it.
pub(crate) const ZST_WORD: &[u8] = &[
    0x28, 0xb5, 0x2f, 0xfd, 0x64, 0xc2, 0x00, 0xbd, 0x00, 0x00, 0x80, 0x7a, 0x65, 0x62, 0x72, 0x61,
    0x20, 0x66, 0x69, 0x73, 0x68, 0x20, 0x66, 0x69, 0x73, 0x68, 0x0a, 0x01, 0x00, 0xaf, 0x56, 0x8b,
    0x17, 0x8b, 0xc5, 0x4e, 0xa7,
];

/// The same for `"zebra fish " * 41 + "\n"`, which holds no `zebrafish`.
pub(crate) const ZST_CLEAN: &[u8] = &[
    0x28, 0xb5, 0x2f, 0xfd, 0x64, 0xc4, 0x00, 0x9d, 0x00, 0x00, 0x60, 0x7a, 0x65, 0x62, 0x72, 0x61,
    0x20, 0x66, 0x69, 0x73, 0x68, 0x20, 0x0a, 0x01, 0x00, 0xb5, 0x56, 0x8b, 0x17, 0x9e, 0xfc, 0x58,
    0x6e,
];

/// The same for `"a" * 200000 + "zebra fish zebrafish\n"`: 54 bytes, so the
/// engine reads its first 54000 and never the word.
pub(crate) const ZST_WORD_PAST_BOUND: &[u8] = &[
    0x28, 0xb5, 0x2f, 0xfd, 0xa4, 0x55, 0x0d, 0x03, 0x00, 0x4c, 0x00, 0x00, 0x08, 0x61, 0x01, 0x00,
    0xfc, 0xff, 0x39, 0x10, 0x02, 0xd5, 0x00, 0x00, 0x80, 0x7a, 0x65, 0x62, 0x72, 0x61, 0x20, 0x66,
    0x69, 0x73, 0x68, 0x20, 0x66, 0x69, 0x73, 0x68, 0x0a, 0x02, 0x00, 0x2e, 0x82, 0xf7, 0x34, 0xe4,
    0x20, 0x08, 0x29, 0xd3, 0x6a, 0xbe,
];

/// The same for `"zebra fish zebrafish\n" + "a" * 200000`: the word lies in the
/// first 45000 bytes the engine reads of its 45.
pub(crate) const ZST_WORD_IN_BOUND: &[u8] = &[
    0x28, 0xb5, 0x2f, 0xfd, 0xa4, 0x55, 0x0d, 0x03, 0x00, 0xcc, 0x00, 0x00, 0x68, 0x7a, 0x65, 0x62,
    0x72, 0x61, 0x20, 0x66, 0x69, 0x73, 0x68, 0x20, 0x0a, 0x61, 0x03, 0x00, 0xe7, 0xff, 0x6f, 0xe2,
    0xcb, 0x82, 0x85, 0xe2, 0x05, 0xab, 0x6a, 0x08, 0x61, 0x81, 0x7e, 0x03, 0x67,
];

/// What the zstd CLI 1.5.7 wrote for `"clean text zebra fish\n"` with a
/// trained dictionary (ID 1028275286). Without it the CLI refuses the frame at
/// its header, before any of its content, as aterm's decoder does.
pub(crate) const ZST_DICTIONARY: &[u8] = &[
    0x28, 0xb5, 0x2f, 0xfd, 0x07, 0x58, 0x56, 0x3c, 0x4a, 0x3d, 0x75, 0x00, 0x00, 0x28, 0x74, 0x65,
    0x78, 0x74, 0x0a, 0x02, 0xcc, 0x19, 0x13, 0x06, 0x11, 0x8c, 0x97, 0xac, 0x87, 0x9e, 0xbc,
];

/// An LSB-first bit writer, DEFLATE's order.
#[derive(Default)]
struct Bits {
    out: Vec<u8>,
    acc: u32,
    n: u32,
}

impl Bits {
    fn put(&mut self, value: u32, len: u32) {
        self.acc |= value << self.n;
        self.n += len;
        while self.n >= 8 {
            self.out.push(self.acc.to_le_bytes()[0]);
            self.acc >>= 8;
            self.n -= 8;
        }
    }

    /// A Huffman code goes in from its most significant bit.
    fn code(&mut self, code: u32, len: u32) {
        self.put(code.reverse_bits() >> (32 - len), len);
    }

    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.out.push(self.acc.to_le_bytes()[0]);
        }
        self.out
    }
}

/// `plain` as one fixed-Huffman DEFLATE block of literals, so no byte run of
/// the compressed stream spells it.
pub(crate) fn deflate(plain: &[u8]) -> Vec<u8> {
    let mut bits = Bits::default();
    bits.put(0b011, 3);
    for &b in plain {
        match b {
            0..=143 => bits.code(0x30 + u32::from(b), 8),
            _ => bits.code(0x190 + u32::from(b) - 144, 9),
        }
    }
    bits.code(0, 7);
    bits.finish()
}

/// A gzip member of `plain`: MTIME `mtime`, and each `(flag, field)` of
/// `fields` (FEXTRA `0x04`, FNAME `0x08`, FCOMMENT `0x10`, in that order).
pub(crate) fn gzip_member(plain: &[u8], mtime: [u8; 4], fields: &[(u8, &[u8])]) -> Vec<u8> {
    let flags = fields.iter().fold(0, |f, &(flag, _)| f | flag);
    let mut out = vec![0x1f, 0x8b, 8, flags];
    out.extend(mtime);
    out.extend([0, 3]);
    for &(flag, field) in fields {
        if flag == 0x04 {
            out.extend(
                u16::try_from(field.len())
                    .expect("a short field")
                    .to_le_bytes(),
            );
            out.extend(field);
        } else {
            out.extend(field);
            out.push(0);
        }
    }
    out.extend(deflate(plain));
    out.extend(Crc32::of(plain).to_le_bytes());
    out.extend(
        u32::try_from(plain.len())
            .expect("a short text")
            .to_le_bytes(),
    );
    out
}

fn stream(kind: &'static str, copy: &[u8], stopped: Option<&str>) -> Plaintext {
    Plaintext::Stream {
        kind,
        copy: copy.to_vec(),
        stopped: stopped.map(str::to_string),
    }
}

fn unjudged(read: &Plaintext) -> bool {
    matches!(read, Plaintext::Unjudged { .. })
}

#[test]
fn a_gzip_file_reads_as_its_members_plaintext_then_their_header_fields() {
    let member = gzip_member(b"one\n", [0; 4], &[]);
    assert!(
        !member.windows(3).any(|w| w == b"one"),
        "the fixture hides its text"
    );
    assert_eq!(read(&member), stream("gzip", b"one\n\n", None));
    let mut two = gzip_member(
        b"one zeb",
        [0; 4],
        &[(0x04, b"xtra"), (0x08, b"name.txt"), (0x10, b"a comment")],
    );
    two.extend(gzip_member(b"rafish\n", [0; 4], &[(0x08, b"second")]));
    assert_eq!(
        read(&two),
        stream(
            "gzip",
            b"one zebrafish\n\nxtra\nname.txt\na comment\nsecond\n",
            None
        ),
        "the plaintext runs on across members; each field gets a line"
    );
}

#[test]
fn a_gzip_stream_that_stops_at_a_header_or_trailer_keeps_what_decoded_before() {
    let good = gzip_member(b"kept\n", [0; 4], &[(0x08, b"n")]);
    let mut bad_crc = good.clone();
    let at = bad_crc.len() - 8;
    bad_crc[at] ^= 1;
    let mut short_len = good.clone();
    let at = short_len.len() - 4;
    short_len[at] ^= 1;
    let mut trailing = good.clone();
    trailing.extend(b"junk");
    let mut reserved = good.clone();
    reserved.extend(gzip_member(b"never\n", [0; 4], &[]));
    let flags = good.len() + 3;
    reserved[flags] = 0x20;
    let mut not_deflate = good.clone();
    not_deflate.extend(gzip_member(b"never\n", [0; 4], &[]));
    not_deflate[good.len() + 2] = 7;
    let truncated = good[..good.len() - 3].to_vec();
    let mut header_crc = good.clone();
    header_crc.extend([0x1f, 0x8b, 8, 0x0a, 0, 0, 0, 0, 0, 3, b'x', 0, 0, 0]);
    for (stream_bytes, copy, why) in [
        (
            &bad_crc,
            &b"kept\n\nn\n"[..],
            "the CRC-32 of the decoded bytes",
        ),
        (&short_len, b"kept\n\nn\n", "the decoded length"),
        (&trailing, b"kept\n\nn\n", "bytes after the last member"),
        (&reserved, b"kept\n\nn\n", "a reserved header flag"),
        (&not_deflate, b"kept\n\nn\n", "compression method 7"),
        (&header_crc, b"kept\n\nn\nx\n", "the header CRC"),
        (&truncated, b"kept\n\nn\n", "ends inside a member"),
    ] {
        match read(stream_bytes) {
            Plaintext::Stream {
                kind: "gzip",
                copy: got,
                stopped: Some(stopped),
            } => {
                assert_eq!(got, copy, "{why}");
                assert!(stopped.contains(why), "{stopped} names {why}");
            }
            other => panic!("{why}: {other:?}"),
        }
    }
}

#[test]
fn a_gzip_stream_broken_inside_its_deflate_data_is_unjudged() {
    let good = gzip_member(b"some text here\n", [0; 4], &[]);
    let body = 10..good.len() - 8;
    assert!(unjudged(&read(&good[..body.start + 3])));
    let mut reserved_block = good.clone();
    reserved_block[body.start] = 0b111;
    assert!(unjudged(&read(&reserved_block)));
}

#[test]
fn a_stream_past_the_engines_bound_reads_as_its_first_bound_bytes() {
    let member = gzip_member(b"0123456789\n", [0; 4], &[(0x08, b"n")]);
    assert_eq!(
        gzip(&member, 4),
        stream("gzip", b"0123\nn\n", Some(&too_large(4)))
    );
    assert_eq!(
        gzip(&member, 11),
        stream("gzip", b"0123456789\n\nn\n", None)
    );
    assert_eq!(engine_bound(ZST_WORD_PAST_BOUND.len()), 54_000);
    let Plaintext::Stream {
        kind: "zstd",
        copy,
        stopped: Some(_),
    } = read(ZST_WORD_PAST_BOUND)
    else {
        panic!("54 bytes of zstd decode past 54000");
    };
    let mut want = vec![b'a'; 54_000];
    want.push(b'\n');
    want.extend(ZST_WORD_PAST_BOUND);
    want.push(b'\n');
    assert!(copy == want, "the first 54000 bytes, then the raw ones");
}

#[test]
fn a_zstd_file_reads_as_its_plaintext_then_its_raw_bytes() {
    let mut want = b"zebra fish ".repeat(40);
    want.extend(b"zebrafish\n\n");
    want.extend(ZST_WORD);
    want.push(b'\n');
    assert_eq!(read(ZST_WORD), stream("zstd", &want, None));
    let mut skippable = vec![0x5a, 0x2a, 0x4d, 0x18, 3, 0, 0, 0];
    skippable.extend(b"raw");
    let mut want = b"\n".to_vec();
    want.extend(&skippable);
    want.push(b'\n');
    assert_eq!(read(&skippable), stream("zstd", &want, None));
    let mut corrupt = ZST_WORD.to_vec();
    let at = corrupt.len() - 2;
    corrupt[at] ^= 0xff;
    assert!(unjudged(&read(&corrupt)), "a checksum the frame fails");
    assert!(unjudged(&read(&ZST_WORD[..20])), "a frame cut short");
}

#[test]
fn a_zstd_stream_refused_between_frames_stops_after_the_frames_before() {
    let then = |tail: &[u8]| [ZST_CLEAN, tail].concat();
    let mut clean = b"zebra fish ".repeat(41);
    clean.push(b'\n');
    for (bytes, plain, why) in [
        (then(b"junkjunk"), &clean[..], "not a frame"),
        (then(b"ju"), &clean, "ends inside a frame"),
        (then(ZST_DICTIONARY), &clean, "needs dictionary"),
        (ZST_DICTIONARY.to_vec(), b"", "needs dictionary"),
        (
            then(&[0x28, 0xb5, 0x2f, 0xfd, 0, 18 << 3]),
            &clean,
            "window",
        ),
    ] {
        let mut want = plain.to_vec();
        want.push(b'\n');
        want.extend(&bytes);
        want.push(b'\n');
        match read(&bytes) {
            Plaintext::Stream {
                kind: "zstd",
                copy,
                stopped: Some(stopped),
            } => {
                assert!(copy == want, "{why}: the whole frames, then the raw bytes");
                assert!(stopped.contains(why), "{stopped} names {why}");
            }
            other => panic!("{why}: {other:?}"),
        }
    }
    for (tail, format) in [
        (gzip_member(b"x\n", [0; 4], &[]), "gzip"),
        (vec![0x27, 0xb5, 0x2f, 0xfd, 0, 0, 0, 0], "legacy zstd"),
        (vec![0xfd, 0x37, 0x7a, 0x58, 0x5a, 0], "xz"),
        (vec![0x5d, 0, 0, 0x80, 0], "lzma"),
        (0x184d_2204u32.to_le_bytes().to_vec(), "lz4"),
    ] {
        assert!(unjudged(&read(&then(&tail))), "the CLI decodes {format} on");
    }
}

#[test]
fn a_bom_names_the_encoding_and_byte_order_and_what_does_not_decode_is_dropped() {
    let utf16 = |little: bool, units: &[u16]| -> Vec<u8> {
        let mut out: Vec<u8> = if little {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        for u in units {
            out.extend(if little {
                u.to_le_bytes()
            } else {
                u.to_be_bytes()
            });
        }
        out
    };
    let units: Vec<u16> = "a\u{1f600}b\n".encode_utf16().collect();
    for little in [true, false] {
        let bom = |text: &str| Plaintext::Bom {
            encoding: "utf-16",
            text: text.to_string(),
        };
        assert_eq!(read(&utf16(little, &units)), bom("a\u{1f600}b\n"));
        let mut odd = utf16(little, &[0x61, 0xd800, 0x62, 0xdc00, 0x63]);
        odd.push(0x64);
        assert_eq!(read(&odd), bom("abc"), "a lone surrogate and an odd byte");
        let pair = utf16(little, &[0xd800, 0xd801, 0xdc00, 0x61]);
        assert_eq!(
            read(&pair),
            bom("\u{10400}a"),
            "a high surrogate before a pair"
        );
    }
    let mut le = vec![0xff, 0xfe, 0, 0];
    let mut be = vec![0, 0, 0xfe, 0xff];
    for c in [0x61u32, 0x11_0000, 0xd800, 0x1f600] {
        le.extend(c.to_le_bytes());
        be.extend(c.to_be_bytes());
    }
    le.push(0x62);
    for bytes in [le, be] {
        assert_eq!(
            read(&bytes),
            Plaintext::Bom {
                encoding: "utf-32",
                text: "a\u{1f600}".into()
            }
        );
    }
    assert_eq!(read(b"\xef\xbb\xbfplain"), Plaintext::None, "UTF-8's BOM");
    assert_eq!(read(b"plain"), Plaintext::None);
}
