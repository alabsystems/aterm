// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Archive, EntryType, MAX_EXTENSION_BODY};
use std::io::Read;

/// Write `bytes` at `off` in a header block, clipped at the block's end.
fn put(h: &mut [u8; 512], off: usize, bytes: &[u8]) {
    for (i, b) in bytes.iter().enumerate() {
        if let Some(slot) = h.get_mut(off + i) {
            *slot = *b;
        }
    }
}

/// Fill in a header's checksum field as the unsigned sum of all 512 bytes (with the field as spaces).
fn seal(h: &mut [u8; 512]) {
    h[148..156].fill(b' ');
    let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
    put(h, 148, format!("{sum:06o}\0 ").as_bytes());
}

/// A POSIX ustar header.
fn header(name: &str, flag: u8, size: u64) -> [u8; 512] {
    header_magic(name, flag, size, b"ustar\0", b"00")
}

/// A header with the magic and version chosen by the caller.
fn header_magic(name: &str, flag: u8, size: u64, magic: &[u8], version: &[u8]) -> [u8; 512] {
    let mut h = [0u8; 512];
    put(&mut h, 0, name.as_bytes());
    put(&mut h, 100, b"0000644\0");
    put(&mut h, 108, b"0000000\0");
    put(&mut h, 116, b"0000000\0");
    put(&mut h, 124, format!("{size:011o}\0").as_bytes());
    put(&mut h, 136, b"00000000000\0");
    h[156] = flag;
    put(&mut h, 257, magic);
    put(&mut h, 263, version);
    seal(&mut h);
    h
}

/// Content padded up to the next block.
fn padded(body: &[u8]) -> Vec<u8> {
    let mut v = body.to_vec();
    let rem = v.len() % 512;
    if rem != 0 {
        v.resize(v.len() + (512 - rem), 0);
    }
    v
}

/// The two zero blocks that end an archive.
fn end() -> Vec<u8> {
    vec![0u8; 1024]
}

/// One PAX `"<len> key=value\n"` record, with its self-describing length.
fn pax(key: &str, value: &str) -> Vec<u8> {
    let payload = format!("{key}={value}\n");
    let mut len = payload.len() + 2;
    loop {
        let candidate = format!("{len} {payload}");
        if candidate.len() == len {
            return candidate.into_bytes();
        }
        len = candidate.len();
    }
}

/// One entry, reduced to what the extractor reads.
#[derive(Debug, PartialEq, Eq)]
struct Seen {
    path: String,
    kind: EntryType,
    size: u64,
    link: Option<String>,
    content: Vec<u8>,
}

/// Walk an archive to the end, or to the first error.
fn walk(bytes: &[u8]) -> Result<Vec<Seen>, String> {
    walk_reader(bytes)
}

/// [`walk`] over any reader, so the short-read paths can be driven too.
fn walk_reader<R: Read>(reader: R) -> Result<Vec<Seen>, String> {
    let mut archive = Archive::new(reader);
    let mut entries = archive.entries().map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    loop {
        let Some(mut entry) = entries.next_entry().map_err(|e| e.to_string())? else {
            return Ok(out);
        };
        let path = entry
            .path()
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .into_owned();
        let kind = entry.header().entry_type();
        let size = entry.entry_size();
        let link = entry
            .link_name()
            .map_err(|e| e.to_string())?
            .map(|l| l.to_string_lossy().into_owned());
        let mut content = Vec::new();
        entry.read_to_end(&mut content).map_err(|e| e.to_string())?;
        out.push(Seen {
            path,
            kind,
            size,
            link,
            content,
        });
    }
}

/// A reader that hands out at most `chunk` bytes per `read` call.
struct Dribble<'a> {
    bytes: &'a [u8],
    chunk: usize,
}

impl Read for Dribble<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.bytes.len().min(buf.len()).min(self.chunk);
        buf[..n].copy_from_slice(&self.bytes[..n]);
        self.bytes = &self.bytes[n..];
        Ok(n)
    }
}

/// The shapes a real bundle is made of, decoded to exact expected values.
#[test]
fn golden_walk_of_an_ordinary_archive() {
    let mut v = Vec::new();
    v.extend_from_slice(&header("bin/aterm", b'0', 7));
    v.extend_from_slice(&padded(b"payload"));
    v.extend_from_slice(&header("share/", b'5', 0));
    v.extend_from_slice(&header("alias", b'2', 0));
    let mut sym = header("alias", b'2', 0);
    put(&mut sym, 157, b"bin/aterm");
    seal(&mut sym);
    v.truncate(v.len() - 512);
    v.extend_from_slice(&sym);
    v.extend_from_slice(&header("empty.txt", b'0', 0));
    v.extend_from_slice(&end());

    let seen = walk(&v).expect("walk");
    assert_eq!(
        seen,
        vec![
            Seen {
                path: "bin/aterm".into(),
                kind: EntryType::Regular,
                size: 7,
                link: None,
                content: b"payload".to_vec(),
            },
            Seen {
                path: "share/".into(),
                kind: EntryType::Directory,
                size: 0,
                link: None,
                content: Vec::new(),
            },
            Seen {
                path: "alias".into(),
                kind: EntryType::Symlink,
                size: 0,
                link: Some("bin/aterm".into()),
                content: Vec::new(),
            },
            Seen {
                path: "empty.txt".into(),
                kind: EntryType::Regular,
                size: 0,
                link: None,
                content: Vec::new(),
            },
        ]
    );
}

/// A ustar `prefix` joins to `name` with a slash — and only on a POSIX header.
#[test]
fn golden_prefix_is_posix_ustar_only() {
    for (magic, version, expected) in [
        (&b"ustar\0"[..], &b"00"[..], "some/prefix/tail.txt"),
        (&b"ustar "[..], &b" \0"[..], "tail.txt"),
        (&b"ustar "[..], &b"00"[..], "tail.txt"),
        (&b"xxxxx\0"[..], &b"00"[..], "tail.txt"),
    ] {
        let mut h = header_magic("tail.txt", b'0', 0, magic, version);
        put(&mut h, 345, b"some/prefix");
        seal(&mut h);
        let seen = walk(&[&h[..], &end()].concat()).expect("walk");
        assert_eq!(seen.len(), 1);
        assert_eq!(
            seen[0].path, expected,
            "magic {magic:?} version {version:?} must resolve to {expected}"
        );
    }
}

/// A GNU long name replaces the header's, and drops one trailing NUL.
#[test]
fn golden_gnu_long_name_drops_one_trailing_nul() {
    for (label, body, expected) in [
        ("terminated", &b"real/name.txt\0"[..], "real/name.txt"),
        ("unterminated", &b"real/name.txt"[..], "real/name.txt"),
        ("two nuls", &b"real/name.txt\0\0"[..], "real/name.txt\u{0}"),
        (
            "interior nul",
            &b"real/\0name.txt\0"[..],
            "real/\u{0}name.txt",
        ),
    ] {
        let mut v = Vec::new();
        v.extend_from_slice(&header("././@LongLink", b'L', body.len() as u64));
        v.extend_from_slice(&padded(body));
        v.extend_from_slice(&header("decoy.txt", b'0', 0));
        v.extend_from_slice(&end());
        let seen = walk(&v).expect("walk");
        assert_eq!(seen.len(), 1, "{label}");
        assert_eq!(seen[0].path, expected, "{label}");
    }
}

/// A PAX `size` record decides the entry's length and where the next header begins.
#[test]
fn golden_pax_size_overrides_the_header_field() {
    let rec = pax("size", "4");
    let mut v = Vec::new();
    v.extend_from_slice(&header("PaxHeader", b'x', rec.len() as u64));
    v.extend_from_slice(&padded(&rec));
    v.extend_from_slice(&header("f.txt", b'0', 512));
    v.extend_from_slice(&padded(&vec![b'Z'; 512]));
    v.extend_from_slice(&header("g.txt", b'0', 3));
    v.extend_from_slice(&padded(b"xyz"));
    v.extend_from_slice(&end());

    let seen = walk(&v).expect("walk");
    assert_eq!(seen.len(), 2, "the size override moved the next header");
    assert_eq!(seen[0].size, 4);
    assert_eq!(seen[0].content, b"ZZZZ");
    assert_eq!(seen[1].path, "g.txt");
    assert_eq!(seen[1].content, b"xyz");
}

/// `Entry::entry_size` is the effective size; `header().entry_size()` is the raw field.
#[test]
fn golden_effective_and_raw_sizes_are_both_reachable() {
    let rec = pax("size", "4");
    let mut v = Vec::new();
    v.extend_from_slice(&header("PaxHeader", b'x', rec.len() as u64));
    v.extend_from_slice(&padded(&rec));
    v.extend_from_slice(&header("f.txt", b'0', 512));
    v.extend_from_slice(&padded(&vec![b'Z'; 512]));
    v.extend_from_slice(&end());

    let mut archive = Archive::new(&v[..]);
    let mut entries = archive.entries().expect("entries");
    let entry = entries.next_entry().expect("ok").expect("some");
    assert_eq!(entry.entry_size(), 4, "effective size");
    assert_eq!(
        entry.header().entry_size().expect("raw size"),
        512,
        "raw header field"
    );
}

/// The first record of a repeated key wins, and a malformed record does not end the scan.
#[test]
fn golden_pax_precedence_and_recovery() {
    let cases: Vec<(&str, Vec<u8>, &str)> = vec![
        (
            "first path wins",
            {
                let mut b = pax("path", "FIRST.txt");
                b.extend_from_slice(&pax("path", "SECOND.txt"));
                b
            },
            "FIRST.txt",
        ),
        (
            "malformed then valid",
            {
                let mut b = b"999 linkpath=y\n".to_vec();
                b.extend_from_slice(&pax("path", "GOOD.txt"));
                b
            },
            "GOOD.txt",
        ),
        (
            "valid then malformed",
            {
                let mut b = pax("path", "GOOD.txt");
                b.extend_from_slice(b"999 linkpath=y\n");
                b
            },
            "GOOD.txt",
        ),
        (
            "blank line stops the scan",
            {
                let mut b = pax("mtime", "0");
                b.push(b'\n');
                b.extend_from_slice(&pax("path", "AFTER.txt"));
                b
            },
            "hdr.txt",
        ),
        (
            "embedded newline is malformed",
            pax("path", "a\nb"),
            "hdr.txt",
        ),
    ];
    for (label, body, expected) in cases {
        let mut v = Vec::new();
        v.extend_from_slice(&header("PaxHeader", b'x', body.len() as u64));
        v.extend_from_slice(&padded(&body));
        v.extend_from_slice(&header("hdr.txt", b'0', 0));
        v.extend_from_slice(&end());
        let seen = walk(&v).expect("walk");
        assert_eq!(seen.len(), 1, "{label}");
        assert_eq!(seen[0].path, expected, "{label}");
    }
}

/// A GNU long name beats a PAX `path` whatever order they arrive in.
#[test]
fn golden_gnu_long_name_beats_a_pax_path_in_both_orders() {
    let gnu = b"gnu.txt\0";
    let px = pax("path", "pax.txt");
    for order in [true, false] {
        let mut v = Vec::new();
        let emit_gnu = |v: &mut Vec<u8>| {
            v.extend_from_slice(&header("././@LongLink", b'L', gnu.len() as u64));
            v.extend_from_slice(&padded(gnu));
        };
        let emit_pax = |v: &mut Vec<u8>| {
            v.extend_from_slice(&header("PaxHeader", b'x', px.len() as u64));
            v.extend_from_slice(&padded(&px));
        };
        if order {
            emit_gnu(&mut v);
            emit_pax(&mut v);
        } else {
            emit_pax(&mut v);
            emit_gnu(&mut v);
        }
        v.extend_from_slice(&header("hdr.txt", b'0', 0));
        v.extend_from_slice(&end());
        let seen = walk(&v).expect("walk");
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].path, "gnu.txt", "gnu long name must win");
    }
}

/// An override that exists but is empty is `Some("")`, not `None`.
#[test]
fn golden_empty_link_override_is_not_absence() {
    let mut v = Vec::new();
    v.extend_from_slice(&header("././@LongLink", b'K', 0));
    v.extend_from_slice(&header("hard", b'1', 0));
    v.extend_from_slice(&end());
    let seen = walk(&v).expect("walk");
    assert_eq!(seen.len(), 1);
    assert_eq!(
        seen[0].link,
        Some(String::new()),
        "empty K body is a target"
    );

    let mut v = Vec::new();
    v.extend_from_slice(&header("hard", b'1', 0));
    v.extend_from_slice(&end());
    let seen = walk(&v).expect("walk");
    assert_eq!(seen[0].link, None);
}

/// A base-256 `size` reads the low eight bytes of the 12-byte field.
#[test]
fn golden_base256_size_reads_the_low_eight_bytes() {
    let mut h = header("x.txt", b'0', 0);
    h[124] = 0x80;
    h[125] = 0x01;
    h[126..135].fill(0);
    h[135] = 4;
    seal(&mut h);
    let seen = walk(&[&h[..], &padded(b"data"), &end()].concat()).expect("walk");
    assert_eq!(seen[0].size, 4, "leading bytes above the low 8 are ignored");
    assert_eq!(seen[0].content, b"data");
}

/// The [`MAX_EXTENSION_BODY`] ceiling.
#[test]
fn extension_body_ceiling_refuses_an_oversized_long_name() {
    let over = MAX_EXTENSION_BODY + 512;
    let mut v = Vec::new();
    v.extend_from_slice(&header("././@LongLink", b'L', over));
    v.extend_from_slice(&padded(&vec![b'n'; usize::try_from(over).expect("fits")]));
    v.extend_from_slice(&header("decoy.txt", b'0', 0));
    v.extend_from_slice(&end());
    let err = walk(&v).expect_err("an oversized extension body must be refused");
    assert!(err.contains("implausibly large"), "unexpected error: {err}");

    let under = MAX_EXTENSION_BODY - 512;
    let mut body = vec![b'n'; usize::try_from(under).expect("fits")];
    body.push(0);
    body.truncate(usize::try_from(under).expect("fits"));
    let mut v = Vec::new();
    v.extend_from_slice(&header("././@LongLink", b'L', under));
    v.extend_from_slice(&padded(&body));
    v.extend_from_slice(&header("decoy.txt", b'0', 0));
    v.extend_from_slice(&end());
    let seen = walk(&v).expect("a body under the ceiling must be read");
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].path.len(), usize::try_from(under).expect("fits"));
}

/// The checksum is the unsigned byte sum only.
#[test]
fn a_signed_only_checksum_is_refused() {
    let mut h = header("x.txt", b'0', 0);
    h[5] = 0x80;
    h[148..156].fill(b' ');
    let signed: i64 = h.iter().map(|&b| i64::from(b as i8)).sum();
    let unsigned: i64 = h.iter().map(|&b| i64::from(b)).sum();
    assert_ne!(
        signed, unsigned,
        "the fixture must distinguish the two rules"
    );
    put(
        &mut h,
        148,
        format!("{:06o}\0 ", u64::try_from(signed).expect("positive")).as_bytes(),
    );
    let err = walk(&[&h[..], &end()].concat()).expect_err("signed-only checksum");
    assert!(err.contains("checksum"), "unexpected error: {err}");
}

/// A one-byte-at-a-time reader decodes identically to a full buffer.
#[test]
fn a_dribbling_reader_decodes_identically() {
    let rec = pax("size", "4");
    let mut v = Vec::new();
    v.extend_from_slice(&header("PaxHeader", b'x', rec.len() as u64));
    v.extend_from_slice(&padded(&rec));
    v.extend_from_slice(&header("f.txt", b'0', 512));
    v.extend_from_slice(&padded(&vec![b'Z'; 512]));
    v.extend_from_slice(&header("././@LongLink", b'L', 14));
    v.extend_from_slice(&padded(b"real/name.txt\0"));
    v.extend_from_slice(&header("decoy.txt", b'0', 3));
    v.extend_from_slice(&padded(b"abc"));
    v.extend_from_slice(&end());

    let whole = walk(&v).expect("walk");
    for chunk in [1usize, 3, 7, 511, 512, 513] {
        let dribbled = walk_reader(Dribble { bytes: &v, chunk }).expect("dribbled walk");
        assert_eq!(dribbled, whole, "chunk size {chunk} changed the decode");
    }
}

/// Truncation inside an entry or header is an error; between entries it is a clean end.
#[test]
fn truncation_is_an_error_inside_an_entry_and_clean_between_them() {
    let mut v = Vec::new();
    v.extend_from_slice(&header("a.txt", b'0', 600));
    v.extend_from_slice(&padded(&vec![b'A'; 600]));
    v.extend_from_slice(&header("b.txt", b'0', 4));
    v.extend_from_slice(&padded(b"BBBB"));
    v.extend_from_slice(&end());

    assert!(walk(&v[..900]).is_err(), "cut inside content");
    assert!(walk(&v[..1800]).is_err(), "cut inside a header");
    let seen = walk(&v[..1536]).expect("cut between entries");
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].path, "a.txt");
    assert_eq!(walk(&[]).expect("empty"), Vec::new());
}

/// An extension header describing an entry that never arrives is refused at EOF and at the end marker.
#[test]
fn a_dangling_extension_header_is_refused() {
    for (label, tail) in [("eof", Vec::new()), ("end marker", end())] {
        for flag in *b"LKx" {
            let body = b"dangling\0".to_vec();
            let mut v = Vec::new();
            v.extend_from_slice(&header("././@LongLink", flag, body.len() as u64));
            v.extend_from_slice(&padded(&body));
            v.extend_from_slice(&tail);
            let err = walk(&v).expect_err("dangling extension");
            assert!(
                err.contains("never arrives"),
                "{label}/{}: unexpected error: {err}",
                flag as char
            );
        }
    }
}

/// An extension typeflag with neither magic is an ordinary entry, as is any `g` header.
#[test]
fn unrecognized_and_global_headers_are_ordinary_entries() {
    let body = b"real/name.txt\0".to_vec();
    let mut v = Vec::new();
    let mut h = header_magic("././@LongLink", b'L', body.len() as u64, b"xxxxx\0", b"00");
    seal(&mut h);
    v.extend_from_slice(&h);
    v.extend_from_slice(&padded(&body));
    v.extend_from_slice(&header("decoy.txt", b'0', 5));
    v.extend_from_slice(&padded(b"decoy"));
    v.extend_from_slice(&end());
    let seen = walk(&v).expect("walk");
    assert_eq!(seen.len(), 2, "a magic-less L must not be consumed");
    assert_eq!(seen[0].kind, EntryType::Other);
    assert_eq!(seen[1].path, "decoy.txt", "the name must NOT be overridden");

    for (magic, version) in [
        (&b"ustar\0"[..], &b"00"[..]),
        (&b"ustar "[..], &b" \0"[..]),
        (&b"xxxxx\0"[..], &b"00"[..]),
    ] {
        let body = pax("path", "global/override.txt");
        let mut v = Vec::new();
        v.extend_from_slice(&header_magic(
            "GlobalHead",
            b'g',
            body.len() as u64,
            magic,
            version,
        ));
        v.extend_from_slice(&padded(&body));
        v.extend_from_slice(&header("plain.txt", b'0', 0));
        v.extend_from_slice(&end());
        let seen = walk(&v).expect("walk");
        assert_eq!(seen.len(), 2, "a g header is an entry, not an extension");
        assert_eq!(seen[0].kind, EntryType::Other);
        assert_eq!(seen[1].path, "plain.txt", "a g header must not rename");
    }
}

/// `mode` and `chksum` are octal-only; a high-bit value in either is rejected.
#[test]
fn mode_and_checksum_reject_the_base256_form() {
    let mut h = header("x.txt", b'0', 0);
    h[100] = 0x80;
    h[101..107].fill(0);
    h[107] = 0o355;
    seal(&mut h);
    let bytes = [&h[..], &end()].concat();
    let mut archive = Archive::new(&bytes[..]);
    let mut entries = archive.entries().expect("entries");
    let entry = entries.next_entry().expect("ok").expect("some");
    assert!(
        entry.header().mode().is_err(),
        "a base-256 mode must not parse as a number"
    );

    let mut h = header("x.txt", b'0', 0);
    h[100..108].copy_from_slice(b"+000755\0");
    seal(&mut h);
    let bytes = [&h[..], &end()].concat();
    let mut archive = Archive::new(&bytes[..]);
    let mut entries = archive.entries().expect("entries");
    let entry = entries.next_entry().expect("ok").expect("some");
    assert!(
        entry.header().mode().is_err(),
        "a plus-prefixed mode must not parse as octal"
    );

    let mut h = header("x.txt", b'0', 0);
    h[148..156].fill(b' ');
    let sum: u64 = h.iter().map(|&b| u64::from(b)).sum();
    h[148] = 0x80;
    for (i, shift) in (0..7).enumerate() {
        h[155 - i] = ((sum >> (8 * shift)) & 0xff) as u8;
    }
    let err = walk(&[&h[..], &end()].concat()).expect_err("base-256 checksum");
    assert!(err.contains("checksum"), "unexpected error: {err}");
}

/// A size within a block of `u64::MAX` is refused before the entry is yielded.
#[test]
fn an_unadvanceable_size_is_refused_before_the_entry() {
    let mut h = header("x.txt", b'0', 0);
    h[124] = 0x80;
    h[125..136].fill(0xff);
    seal(&mut h);
    let err = walk(&[&h[..], &end()].concat()).expect_err("size overflow");
    assert!(err.contains("overflow"), "unexpected error: {err}");
}

/// An entry hands out only its declared bytes, and unread content is skipped before the next header.
#[test]
fn content_is_bounded_and_unread_content_is_skipped() {
    let mut v = Vec::new();
    v.extend_from_slice(&header("skipme.bin", b'0', 1000));
    v.extend_from_slice(&padded(&vec![7u8; 1000]));
    v.extend_from_slice(&header("after.txt", b'0', 5));
    v.extend_from_slice(&padded(b"after"));
    v.extend_from_slice(&end());

    let mut archive = Archive::new(&v[..]);
    let mut entries = archive.entries().expect("entries");
    let first = entries.next_entry().expect("ok").expect("some");
    assert_eq!(first.entry_size(), 1000);
    drop(first);
    let mut second = entries.next_entry().expect("ok").expect("some");
    let mut body = Vec::new();
    second.read_to_end(&mut body).expect("read");
    assert_eq!(body, b"after");
    drop(second);
    assert!(entries.next_entry().expect("ok").is_none());
    assert!(entries.next_entry().expect("ok").is_none());
}
