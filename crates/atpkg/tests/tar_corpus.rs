// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Corpus tests for [`atpkg::tarread`], the first-party read-only tar parser.

use std::cell::Cell;
use std::io::Read;
use std::path::PathBuf;

/// One entry, reduced to exactly what `atpkg::extract` reads out of it.
#[derive(Debug, PartialEq, Eq)]
struct Record {
    path: PathBuf,
    kind: &'static str,
    mode: u32,
    /// The EFFECTIVE content length — after a PAX `size` record has overridden
    /// the header field.
    size: u64,
    /// The RAW `size` field, unoverridden.
    header_size: u64,
    link: Option<PathBuf>,
    content: Vec<u8>,
}

/// The outcome of walking an archive: the entries decoded before anything went
/// wrong, and whether anything did.
#[derive(Debug, PartialEq, Eq)]
struct Walk {
    entries: Vec<Record>,
    errored: bool,
}

/// Walk with the FIRST-PARTY reader.
fn walk_mine(bytes: &[u8]) -> Walk {
    use atpkg::tarread::{Archive, EntryType};
    let mut archive = Archive::new(bytes);
    let mut entries = Vec::new();
    let mut src = match archive.entries() {
        Ok(e) => e,
        Err(_) => {
            return Walk {
                entries,
                errored: true,
            };
        }
    };
    loop {
        let next = match src.next_entry() {
            Ok(v) => v,
            Err(_) => {
                return Walk {
                    entries,
                    errored: true,
                };
            }
        };
        let Some(mut entry) = next else {
            return Walk {
                entries,
                errored: false,
            };
        };
        let kind = match entry.header().entry_type() {
            EntryType::Regular => "regular",
            EntryType::Continuous => "continuous",
            EntryType::Directory => "directory",
            EntryType::Symlink => "symlink",
            EntryType::Link => "link",
            EntryType::Other => "other",
        };
        let size = entry.entry_size();
        let (Ok(path), Ok(mode), Ok(header_size), Ok(link)) = (
            entry.path().map(|p| p.into_owned()),
            entry.header().mode(),
            entry.header().entry_size(),
            entry.link_name().map(|l| l.map(|p| p.into_owned())),
        ) else {
            return Walk {
                entries,
                errored: true,
            };
        };
        let mut content = Vec::new();
        if entry.read_to_end(&mut content).is_err() {
            return Walk {
                entries,
                errored: true,
            };
        }
        entries.push(Record {
            path,
            kind,
            mode,
            size,
            header_size,
            link,
            content,
        });
    }
}

thread_local! {
    static GOLDEN: Cell<u64> = const { Cell::new(0xcbf2_9ce4_8422_2325) };
}

/// Fold `bytes` (and a separator) into this test thread's digest.
fn feed(bytes: &[u8]) {
    GOLDEN.with(|g| {
        let mut h = g.get();
        for &b in bytes.iter().chain(&[0xff]) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        g.set(h);
    });
}

fn feed_walk(walk: &Walk) {
    for e in &walk.entries {
        feed(e.path.as_os_str().as_encoded_bytes());
        feed(e.kind.as_bytes());
        feed(&e.mode.to_le_bytes());
        feed(&e.size.to_le_bytes());
        feed(&e.header_size.to_le_bytes());
        match &e.link {
            Some(l) => feed(l.as_os_str().as_encoded_bytes()),
            None => feed(b"\0none"),
        }
        feed(&e.content);
    }
    feed(&[u8::from(walk.errored)]);
}

/// Walk `bytes` and fold the result into the digest.
fn record(bytes: &[u8]) {
    feed_walk(&walk_mine(bytes));
}

/// Assert this thread's digest, then reset it.
#[track_caller]
fn pin(expected: &str) {
    let got = GOLDEN.with(|g| g.replace(0xcbf2_9ce4_8422_2325));
    assert_eq!(format!("{got:016x}"), expected, "the pinned corpus changed");
}

/// Write `s` at `off` in a header block, clipped at the block's end.
fn put(h: &mut [u8; 512], off: usize, s: &[u8]) {
    for (i, b) in s.iter().enumerate() {
        if let Some(slot) = h.get_mut(off + i) {
            *slot = *b;
        }
    }
}

/// A raw 512-byte USTAR header, so a test can write bytes no honest writer
/// would (the same trick `extract.rs`'s own fixtures use).
fn raw_header(name: &str, typeflag: u8, linkname: &str, size: u64, mode: u32) -> [u8; 512] {
    raw_header_magic(name, typeflag, linkname, size, mode, b"ustar\0", b"00")
}

/// [`raw_header`] with the magic and version chosen by the caller, separating
/// POSIX ustar (`"ustar\0"` + `"00"`) from GNU (`"ustar "` + `" \0"`) and
/// headers that match neither.
fn raw_header_magic(
    name: &str,
    typeflag: u8,
    linkname: &str,
    size: u64,
    mode: u32,
    magic: &[u8],
    version: &[u8],
) -> [u8; 512] {
    let mut h = [0u8; 512];
    put(&mut h, 0, name.as_bytes());
    put(&mut h, 100, format!("{mode:07o}\0").as_bytes());
    put(&mut h, 108, b"0000000\0");
    put(&mut h, 116, b"0000000\0");
    put(&mut h, 124, format!("{size:011o}\0").as_bytes());
    put(&mut h, 136, b"00000000000\0");
    h[156] = typeflag;
    put(&mut h, 157, linkname.as_bytes());
    put(&mut h, 257, magic);
    put(&mut h, 263, version);
    checksum(&mut h);
    h
}

/// Fill in a header's checksum field the way every writer does.
fn checksum(h: &mut [u8; 512]) {
    h[148..156].fill(b' ');
    let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
    let s = format!("{sum:06o}\0 ");
    for (i, b) in s.bytes().enumerate() {
        if let Some(slot) = h.get_mut(148 + i) {
            *slot = b;
        }
    }
}

/// Content padded to the next 512-byte block.
fn padded(body: &[u8]) -> Vec<u8> {
    let mut v = body.to_vec();
    let rem = v.len() % 512;
    if rem != 0 {
        v.resize(v.len() + (512 - rem), 0);
    }
    v
}

/// The two zero blocks that end an archive.
fn end_marker() -> Vec<u8> {
    vec![0u8; 1024]
}

/// A small ustar archive in the shape a writer emits — a path too long for
/// the `name` field split into `prefix` — the "known good" shape every
/// mutation starts from.
fn ustar_archive() -> Vec<u8> {
    let mut v = Vec::new();
    for (name, body, mode) in [
        ("bin/aterm", &b"ELF-ish payload"[..], 0o755),
        ("share/doc/readme.txt", &b"hello"[..], 0o644),
        ("empty.txt", &b""[..], 0o644),
        (
            "a/very/deeply/nested/path/that/goes/on/and/on/and/on/for/rather/a/long/way/indeed/so/that/the/hundred/byte/name/field/cannot/hold/it.txt",
            &b"long name"[..],
            0o644,
        ),
    ] {
        let split = name
            .match_indices('/')
            .map(|(i, _)| i)
            .find(|&i| name.len() - i - 1 <= 100)
            .filter(|_| name.len() > 100);
        let mut h = match split {
            Some(i) => {
                let mut h = raw_header(&name[i + 1..], b'0', "", body.len() as u64, mode);
                put(&mut h, 345, &name.as_bytes()[..i]);
                checksum(&mut h);
                h
            }
            None => raw_header(name, b'0', "", body.len() as u64, mode),
        };
        put(&mut h, 108, b"0000000\0");
        v.extend_from_slice(&h);
        v.extend_from_slice(&padded(body));
    }
    v.extend_from_slice(&raw_header("share/doc/", b'5', "", 0, 0o755));
    v.extend_from_slice(&end_marker());
    v
}

/// A hand-built archive exercising every entry type the extractor classifies,
/// plus a GNU long name and a PAX `path` record — the two extension forms real
/// system tars emit.
fn handmade_archive() -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&raw_header("plain.txt", b'0', "", 5, 0o644));
    v.extend_from_slice(&padded(b"12345"));
    v.extend_from_slice(&raw_header("dir/", b'5', "", 0, 0o755));
    v.extend_from_slice(&raw_header("link", b'2', "plain.txt", 0, 0o777));
    v.extend_from_slice(&raw_header("hard", b'1', "plain.txt", 0, 0o644));
    v.extend_from_slice(&raw_header("cont.txt", b'7', "", 3, 0o644));
    v.extend_from_slice(&padded(b"abc"));
    v.extend_from_slice(&raw_header("fifo", b'6', "", 0, 0o644));
    let long = "g/".repeat(80) + "name.txt";
    let body = {
        let mut b = long.clone().into_bytes();
        b.push(0);
        b
    };
    v.extend_from_slice(&raw_header(
        "././@LongLink",
        b'L',
        "",
        body.len() as u64,
        0o644,
    ));
    v.extend_from_slice(&padded(&body));
    v.extend_from_slice(&raw_header("truncated-name", b'0', "", 2, 0o644));
    v.extend_from_slice(&padded(b"gg"));
    let paxpath = "p/".repeat(90) + "pax.txt";
    let pax = pax_record("path", &paxpath);
    v.extend_from_slice(&raw_header("PaxHeader", b'x', "", pax.len() as u64, 0o644));
    v.extend_from_slice(&padded(&pax));
    v.extend_from_slice(&raw_header("short-name", b'0', "", 3, 0o644));
    v.extend_from_slice(&padded(b"pax"));
    v.extend_from_slice(&end_marker());
    v
}

/// One PAX `"<len> key=value\n"` record, with the self-describing length that
/// makes the format its own parsing hazard.
fn pax_record(key: &str, value: &str) -> Vec<u8> {
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

/// SplitMix64 — identical corpus on every machine and every run.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next_u64() % n as u64) as usize
        }
    }
}

#[test]
fn ustar_archive_is_pinned() {
    let walk = walk_mine(&ustar_archive());
    assert!(!walk.errored);
    assert_eq!(walk.entries.len(), 5);
    assert!(walk.entries[3].path.ends_with("cannot/hold/it.txt"));
    assert!(walk.entries[3].path.starts_with("a/very/deeply"));
    record(&ustar_archive());
    pin(USTAR_ARCHIVE);
}

#[test]
fn every_entry_type_and_extension_is_pinned() {
    record(&handmade_archive());
    pin(EVERY_ENTRY_TYPE);
}

#[test]
fn empty_and_marker_only_archives_are_pinned() {
    record(&[]);
    record(&end_marker());
    record(&vec![0u8; 512]);
    record(&vec![0u8; 511]);
    record(&vec![0u8; 2048]);
    pin(EMPTY_AND_MARKER_ONLY_ARCHIVES);
}

#[test]
fn truncation_at_every_boundary_is_pinned() {
    for source in [ustar_archive(), handmade_archive()] {
        let mut cuts: Vec<usize> = (0..=source.len()).step_by(512).collect();
        cuts.extend([1, 7, 100, 148, 156, 257, 511, 513, 700, 1023]);
        for cut in cuts {
            if cut > source.len() {
                continue;
            }
            record(&source[..cut]);
        }
    }
    pin(TRUNCATION);
}

#[test]
fn adversarial_headers_are_pinned() {
    let mut bad_sum = raw_header("x", b'0', "", 0, 0o644);
    bad_sum[148] = b'1';
    record(&[&bad_sum[..], &end_marker()].concat());

    let mut bad_size = raw_header("x", b'0', "", 0, 0o644);
    bad_size[124..136].copy_from_slice(b"9999999999\0\0");
    checksum(&mut bad_size);
    record(&[&bad_size[..], &end_marker()].concat());

    let mut b256 = raw_header("x", b'0', "", 0, 0o644);
    b256[124] = 0x80 | 0x7f;
    b256[125..136].fill(0xff);
    checksum(&mut b256);
    record(&[&b256[..], &end_marker()].concat());

    let mut b256_ok = raw_header("x", b'0', "", 0, 0o644);
    b256_ok[124] = 0x80;
    b256_ok[125..135].fill(0);
    b256_ok[135] = 4;
    checksum(&mut b256_ok);
    record(&[&b256_ok[..], &padded(b"data"), &end_marker()].concat());

    let mut huge = raw_header("x", b'0', "", 0, 0o644);
    huge[124..136].copy_from_slice(b"77777777777\0");
    checksum(&mut huge);
    record(&[&huge[..], &end_marker()].concat());

    let mut prefixed = raw_header("tail.txt", b'0', "", 0, 0o644);
    let pre = b"some/long/prefix/path";
    prefixed[345..345 + pre.len()].copy_from_slice(pre);
    checksum(&mut prefixed);
    record(&[&prefixed[..], &end_marker()].concat());

    let empty_name = raw_header("", b'0', "", 0, 0o644);
    record(&[&empty_name[..], &end_marker()].concat());
    pin(ADVERSARIAL_HEADERS);
}

#[test]
fn malformed_pax_records_are_pinned() {
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("length longer than body", b"999 path=x\n".to_vec()),
        ("length shorter than prefix", b"1 path=x\n".to_vec()),
        ("no space separator", b"12path=x\n".to_vec()),
        ("non-numeric length", b"abc path=x\n".to_vec()),
        ("no newline", b"10 path=xxx".to_vec()),
        ("no equals sign", b"9 pathxx\n".to_vec()),
        ("empty body", Vec::new()),
        ("zero length record", b"0 path=x\n".to_vec()),
        ("two records, second malformed", {
            let mut v = pax_record("path", "ok.txt");
            v.extend_from_slice(b"999 linkpath=y\n");
            v
        }),
        ("path with an embedded newline", pax_record("path", "a\nb")),
        ("empty path value", pax_record("path", "")),
    ];
    for (_, body) in cases {
        let mut v = Vec::new();
        v.extend_from_slice(&raw_header("PaxHeader", b'x', "", body.len() as u64, 0o644));
        v.extend_from_slice(&padded(&body));
        v.extend_from_slice(&raw_header("real.txt", b'0', "", 2, 0o644));
        v.extend_from_slice(&padded(b"hi"));
        v.extend_from_slice(&end_marker());
        record(&v);
    }
    pin(MALFORMED_PAX_RECORDS);
}

#[test]
fn malformed_gnu_long_names_are_pinned() {
    let body = b"dangling\0".to_vec();
    let mut v = Vec::new();
    v.extend_from_slice(&raw_header(
        "././@LongLink",
        b'L',
        "",
        body.len() as u64,
        0o644,
    ));
    v.extend_from_slice(&padded(&body));
    v.extend_from_slice(&end_marker());
    record(&v);

    let target = "t/".repeat(70) + "target";
    let mut body = target.into_bytes();
    body.push(0);
    let mut v = Vec::new();
    v.extend_from_slice(&raw_header(
        "././@LongLink",
        b'K',
        "",
        body.len() as u64,
        0o644,
    ));
    v.extend_from_slice(&padded(&body));
    v.extend_from_slice(&raw_header("sym", b'2', "short", 0, 0o777));
    v.extend_from_slice(&end_marker());
    record(&v);

    let mut v = Vec::new();
    v.extend_from_slice(&raw_header("././@LongLink", b'L', "", 0, 0o644));
    v.extend_from_slice(&raw_header("fallback.txt", b'0', "", 0, 0o644));
    v.extend_from_slice(&end_marker());
    record(&v);
    pin(MALFORMED_GNU_LONG_NAMES);
}

/// A PAX `size` record OVERRIDES the header's `size` field — the whole
/// file-list-substitution vector on the bundle path.
#[test]
fn pax_size_records_are_pinned() {
    let mut v = Vec::new();
    let rec = pax_record("size", "4");
    v.extend_from_slice(&raw_header("PaxHeader", b'x', "", rec.len() as u64, 0o644));
    v.extend_from_slice(&padded(&rec));
    v.extend_from_slice(&raw_header("f.txt", b'0', "", 512, 0o644));
    v.extend_from_slice(&padded(&vec![b'Z'; 512]));
    v.extend_from_slice(&end_marker());
    record(&v);

    let mut v = Vec::new();
    let rec = pax_record("size", "4");
    v.extend_from_slice(&raw_header("PaxHeader", b'x', "", rec.len() as u64, 0o644));
    v.extend_from_slice(&padded(&rec));
    v.extend_from_slice(&raw_header("f.txt", b'0', "", 512, 0o644));
    v.extend_from_slice(&padded(&vec![b'Z'; 512]));
    v.extend_from_slice(&raw_header("g.txt", b'0', "", 3, 0o644));
    v.extend_from_slice(&padded(b"xyz"));
    v.extend_from_slice(&end_marker());
    record(&v);

    let mut v = Vec::new();
    let rec = pax_record("size", "600");
    v.extend_from_slice(&raw_header("PaxHeader", b'x', "", rec.len() as u64, 0o644));
    v.extend_from_slice(&padded(&rec));
    v.extend_from_slice(&raw_header("big.txt", b'0', "", 4, 0o644));
    v.extend_from_slice(&padded(&vec![b'Q'; 600]));
    v.extend_from_slice(&end_marker());
    record(&v);

    for (_, declared, pax_size) in [
        ("directory: header 0, pax 512", 0u64, "512"),
        ("directory: header 512, pax 0", 512u64, "0"),
    ] {
        let mut v = Vec::new();
        let rec = pax_record("size", pax_size);
        v.extend_from_slice(&raw_header("PaxHeader", b'x', "", rec.len() as u64, 0o644));
        v.extend_from_slice(&padded(&rec));
        v.extend_from_slice(&raw_header("d/", b'5', "", declared, 0o755));
        v.extend_from_slice(&padded(&vec![0u8; 512]));
        v.extend_from_slice(&end_marker());
        record(&v);
    }

    let mut v = Vec::new();
    let rec = pax_record("size", "4");
    v.extend_from_slice(&raw_header("PaxHeader", b'x', "", rec.len() as u64, 0o644));
    v.extend_from_slice(&padded(&rec));
    let g = pax_record("path", "global.txt");
    v.extend_from_slice(&raw_header("GlobalHead", b'g', "", g.len() as u64, 0o644));
    v.extend_from_slice(&padded(&g));
    v.extend_from_slice(&raw_header("after.txt", b'0', "", 2, 0o644));
    v.extend_from_slice(&padded(b"hi"));
    v.extend_from_slice(&end_marker());
    record(&v);

    for (_, value) in [
        ("non-numeric", "abc"),
        ("signed", "-4"),
        ("plus-signed", "+4"),
        ("leading space", " 4"),
        ("empty", ""),
        ("hex-looking", "0x10"),
        ("u64 overflow", "99999999999999999999999"),
        ("leading zeros", "0000004"),
    ] {
        let mut v = Vec::new();
        let rec = pax_record("size", value);
        v.extend_from_slice(&raw_header("PaxHeader", b'x', "", rec.len() as u64, 0o644));
        v.extend_from_slice(&padded(&rec));
        v.extend_from_slice(&raw_header("f.txt", b'0', "", 4, 0o644));
        v.extend_from_slice(&padded(b"data"));
        v.extend_from_slice(&end_marker());
        record(&v);
    }

    for (_, body) in [
        ("size then path", {
            let mut b = pax_record("size", "4");
            b.extend_from_slice(&pax_record("path", "renamed.txt"));
            b
        }),
        ("path then size", {
            let mut b = pax_record("path", "renamed.txt");
            b.extend_from_slice(&pax_record("size", "4"));
            b
        }),
        ("size, path, linkpath", {
            let mut b = pax_record("size", "4");
            b.extend_from_slice(&pax_record("path", "renamed.txt"));
            b.extend_from_slice(&pax_record("linkpath", "elsewhere.txt"));
            b
        }),
        ("malformed then size", {
            let mut b = b"999 size=4\n".to_vec();
            b.extend_from_slice(&pax_record("size", "4"));
            b
        }),
        ("two size records", {
            let mut b = pax_record("size", "4");
            b.extend_from_slice(&pax_record("size", "512"));
            b
        }),
    ] {
        let mut v = Vec::new();
        v.extend_from_slice(&raw_header("PaxHeader", b'x', "", body.len() as u64, 0o644));
        v.extend_from_slice(&padded(&body));
        v.extend_from_slice(&raw_header("f.txt", b'0', "", 512, 0o644));
        v.extend_from_slice(&padded(&vec![b'Z'; 512]));
        v.extend_from_slice(&raw_header("tail.txt", b'0', "", 2, 0o644));
        v.extend_from_slice(&padded(b"ok"));
        v.extend_from_slice(&end_marker());
        record(&v);
    }
    pin(PAX_SIZE_RECORDS);
}

/// A PAX GLOBAL (`g`) header is an ORDINARY entry, not an extension header.
#[test]
fn pax_global_headers_are_pinned() {
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("global path", pax_record("path", "global/override.txt")),
        ("global linkpath", pax_record("linkpath", "global/target")),
        ("global size", pax_record("size", "4")),
        ("global empty body", Vec::new()),
        ("global malformed", b"999 path=x\n".to_vec()),
    ];
    for (_, body) in cases {
        let mut v = Vec::new();
        v.extend_from_slice(&raw_header(
            "GlobalHead",
            b'g',
            "",
            body.len() as u64,
            0o644,
        ));
        v.extend_from_slice(&padded(&body));
        v.extend_from_slice(&raw_header("plain.txt", b'0', "", 5, 0o644));
        v.extend_from_slice(&padded(b"12345"));
        v.extend_from_slice(&end_marker());
        record(&v);
    }

    let body = pax_record("path", "dangling.txt");
    let mut v = Vec::new();
    v.extend_from_slice(&raw_header(
        "GlobalHead",
        b'g',
        "",
        body.len() as u64,
        0o644,
    ));
    v.extend_from_slice(&padded(&body));
    v.extend_from_slice(&end_marker());
    record(&v);

    let body = pax_record("path", "global/override.txt");
    let mut v = Vec::new();
    v.extend_from_slice(&raw_header_magic(
        "GlobalHead",
        b'g',
        "",
        body.len() as u64,
        0o644,
        b"ustar ",
        b" \0",
    ));
    v.extend_from_slice(&padded(&body));
    v.extend_from_slice(&raw_header("plain.txt", b'0', "", 5, 0o644));
    v.extend_from_slice(&padded(b"12345"));
    v.extend_from_slice(&end_marker());
    record(&v);
    pin(PAX_GLOBAL_HEADERS);
}

/// Duplicate PAX keys resolve to the FIRST record, and a malformed record does
/// not end the scan.
#[test]
fn duplicate_and_recovered_pax_records_are_pinned() {
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("two path records", {
            let mut b = pax_record("path", "FIRST.txt");
            b.extend_from_slice(&pax_record("path", "SECOND.txt"));
            b
        }),
        ("three path records", {
            let mut b = pax_record("path", "one.txt");
            b.extend_from_slice(&pax_record("path", "two.txt"));
            b.extend_from_slice(&pax_record("path", "three.txt"));
            b
        }),
        ("two linkpath records", {
            let mut b = pax_record("linkpath", "first-target");
            b.extend_from_slice(&pax_record("linkpath", "second-target"));
            b
        }),
        ("malformed THEN valid path", {
            let mut b = b"999 linkpath=y\n".to_vec();
            b.extend_from_slice(&pax_record("path", "GOOD.txt"));
            b
        }),
        ("valid path THEN malformed", {
            let mut b = pax_record("path", "GOOD.txt");
            b.extend_from_slice(b"999 linkpath=y\n");
            b
        }),
        ("malformed sandwich", {
            let mut b = b"7 a=b\n".to_vec();
            b.extend_from_slice(&pax_record("path", "GOOD.txt"));
            b.extend_from_slice(b"4 zz\n");
            b
        }),
        ("empty line in the middle", {
            let mut b = pax_record("path", "BEFORE.txt");
            b.push(b'\n');
            b.extend_from_slice(&pax_record("path", "AFTER.txt"));
            b
        }),
        ("unknown keys around a path", {
            let mut b = pax_record("mtime", "1700000000.123456");
            b.extend_from_slice(&pax_record("uid", "501"));
            b.extend_from_slice(&pax_record("path", "real.txt"));
            b.extend_from_slice(&pax_record("gid", "20"));
            b
        }),
        ("non-utf8 key", {
            let mut b = Vec::new();
            let payload = b"\xff\xfe=value\n";
            let len = payload.len() + 2;
            b.extend_from_slice(format!("{len} ").as_bytes());
            b.extend_from_slice(payload);
            b
        }),
    ];
    for (_, body) in cases {
        let mut v = Vec::new();
        v.extend_from_slice(&raw_header("PaxHeader", b'x', "", body.len() as u64, 0o644));
        v.extend_from_slice(&padded(&body));
        v.extend_from_slice(&raw_header("hdr.txt", b'2', "hdr-target", 0, 0o777));
        v.extend_from_slice(&end_marker());
        record(&v);
    }
    pin(DUPLICATE_AND_RECOVERED_PAX_RECORDS);
}

/// `prefix` is a POSIX-ustar field and nothing else.
#[test]
fn magic_and_version_combinations_are_pinned() {
    let magics: [(&str, &[u8], &[u8]); 6] = [
        ("posix", b"ustar\0", b"00"),
        ("gnu", b"ustar ", b" \0"),
        ("posix magic, gnu version", b"ustar\0", b" \0"),
        ("gnu magic, posix version", b"ustar ", b"00"),
        ("garbage magic", b"xxxxx\0", b"00"),
        ("no magic", b"\0\0\0\0\0\0", b"\0\0"),
    ];
    for (_, magic, version) in magics {
        for (_, prefix) in [
            ("empty prefix", &b""[..]),
            ("path-shaped prefix", &b"EVIL"[..]),
            ("long prefix", &b"some/long/prefix/path"[..]),
            ("binary prefix", &[0x80, 0x01, 0x02, 0x03][..]),
        ] {
            let mut h = raw_header_magic("tail.txt", b'0', "", 0, 0o644, magic, version);
            put(&mut h, 345, prefix);
            checksum(&mut h);
            record(&[&h[..], &end_marker()].concat());
        }
    }
    pin(MAGIC_AND_VERSION_COMBINATIONS);
}

/// `L`/`K`/`x` are extension typeflags only on a header that speaks ustar or
/// GNU.
#[test]
fn magicless_extension_headers_are_pinned() {
    for flag in *b"LKxg" {
        for (_, magic, version) in [
            ("no magic", &b"\0\0\0\0\0\0"[..], &b"\0\0"[..]),
            ("garbage magic", &b"xxxxx\0"[..], &b"00"[..]),
            ("ustar magic, wrong version", &b"ustar\0"[..], &b"01"[..]),
            ("gnu magic, wrong version", &b"ustar "[..], &b"00"[..]),
        ] {
            let body = {
                let mut b = b"real/name.txt".to_vec();
                b.push(0);
                b
            };
            let mut v = Vec::new();
            v.extend_from_slice(&raw_header_magic(
                "././@LongLink",
                flag,
                "",
                body.len() as u64,
                0o644,
                magic,
                version,
            ));
            v.extend_from_slice(&padded(&body));
            v.extend_from_slice(&raw_header("decoy.txt", b'0', "", 5, 0o644));
            v.extend_from_slice(&padded(b"decoy"));
            v.extend_from_slice(&end_marker());
            record(&v);
        }
    }
    pin(MAGICLESS_EXTENSION_HEADERS);
}

/// One row of [`duplicate_extension_headers_are_pinned`]'s table: the
/// case label, then the extension headers to emit before the member as
/// `(typeflag, body)` pairs.
type DuplicateExtensionCase<'a> = (&'a str, Vec<(u8, Vec<u8>)>);

/// A SECOND `L`, `K` or `x` before the same member is two answers to "what is
/// this file called", and last-wins would make the answer depend on which
/// parser you asked.
#[test]
fn duplicate_extension_headers_are_pinned() {
    let long_name = |s: &str| {
        let mut b = s.as_bytes().to_vec();
        b.push(0);
        b
    };
    let cases: Vec<DuplicateExtensionCase<'_>> = vec![
        (
            "two long names",
            vec![
                (b'L', long_name("first/name.txt")),
                (b'L', long_name("second/name.txt")),
            ],
        ),
        (
            "two long links",
            vec![
                (b'K', long_name("first-target")),
                (b'K', long_name("second-target")),
            ],
        ),
        (
            "two pax headers",
            vec![
                (b'x', pax_record("path", "first.txt")),
                (b'x', pax_record("path", "second.txt")),
            ],
        ),
        (
            "long name then long link then long name",
            vec![
                (b'L', long_name("first/name.txt")),
                (b'K', long_name("a-target")),
                (b'L', long_name("second/name.txt")),
            ],
        ),
        (
            "one of each, no duplicates",
            vec![
                (b'L', long_name("the/name.txt")),
                (b'K', long_name("the-target")),
                (b'x', pax_record("path", "pax.txt")),
            ],
        ),
        (
            "pax then long name (gnu wins)",
            vec![
                (b'x', pax_record("path", "pax.txt")),
                (b'L', long_name("gnu.txt")),
            ],
        ),
        (
            "long name then pax (gnu still wins)",
            vec![
                (b'L', long_name("gnu.txt")),
                (b'x', pax_record("path", "pax.txt")),
            ],
        ),
    ];
    for (_, headers) in cases {
        let mut v = Vec::new();
        for (flag, body) in &headers {
            v.extend_from_slice(&raw_header(
                "././@LongLink",
                *flag,
                "",
                body.len() as u64,
                0o644,
            ));
            v.extend_from_slice(&padded(body));
        }
        v.extend_from_slice(&raw_header("decoy.txt", b'2', "decoy-target", 0, 0o777));
        v.extend_from_slice(&end_marker());
        record(&v);
    }
    pin(DUPLICATE_EXTENSION_HEADERS);
}

/// The GNU base-256 numeric extension belongs to the fields that DEFINE it.
#[test]
fn base256_numeric_fields_are_pinned() {
    let mut h = raw_header("x.txt", b'0', "", 0, 0o644);
    h[148..156].fill(b' ');
    let sum: u64 = h.iter().map(|&b| u64::from(b)).sum();
    h[148] = 0x80;
    for (i, shift) in (0..7).enumerate() {
        h[155 - i] = ((sum >> (8 * shift)) & 0xff) as u8;
    }
    record(&[&h[..], &end_marker()].concat());

    let mut h = raw_header("x.txt", b'0', "", 0, 0o644);
    h[100] = 0x80;
    h[101..106].fill(0);
    h[106] = 0o1;
    h[107] = 0o355;
    checksum(&mut h);
    record(&[&h[..], &end_marker()].concat());

    let mut h = raw_header("x.txt", b'0', "", 0, 0o644);
    h[124] = 0x80;
    h[125] = 0x01;
    h[126..135].fill(0);
    h[135] = 4;
    checksum(&mut h);
    record(&[&h[..], &padded(b"data"), &end_marker()].concat());

    let mut h = raw_header("x.txt", b'0', "", 0, 0o644);
    h[124] = 0x80;
    h[125] = 0x7f;
    h[126] = 0xff;
    h[127] = 0xff;
    h[128..136].fill(0);
    checksum(&mut h);
    record(&[&h[..], &end_marker()].concat());

    for (_, off, len) in [
        ("uid", 108usize, 8usize),
        ("gid", 116, 8),
        ("mtime", 136, 12),
    ] {
        let mut h = raw_header("x.txt", b'0', "", 0, 0o644);
        h[off..off + len].fill(0);
        h[off] = 0x80;
        h[off + len - 1] = 7;
        checksum(&mut h);
        record(&[&h[..], &end_marker()].concat());
    }

    let mut h = raw_header("x.txt", b'0', "", 0, 0o644);
    h[124] = 0x80;
    h[125..136].fill(0xff);
    checksum(&mut h);
    record(&[&h[..], &end_marker()].concat());
    pin(BASE256_NUMERIC_FIELDS);
}

/// GNU SPARSE (`S`) entries are refused outright, where the `tar` crate parsed
/// the sparse block map: a sparse entry classifies as `Other` and `vet_entry`
/// aborts the whole staged group on one anyway, so a second content-assembly
/// path would buy nothing.
#[test]
fn gnu_sparse_entries_are_refused() {
    let mut sparse_cases: Vec<(&str, Vec<u8>)> = Vec::new();

    let mut v = Vec::new();
    v.extend_from_slice(&raw_header_magic(
        "sparse.bin",
        b'S',
        "",
        512,
        0o644,
        b"ustar ",
        b" \0",
    ));
    v.extend_from_slice(&padded(&vec![b'S'; 512]));
    v.extend_from_slice(&end_marker());
    sparse_cases.push(("gnu magic, empty sparse map", v));

    let mut h = raw_header_magic("sparse.bin", b'S', "", 512, 0o644, b"ustar ", b" \0");
    put(&mut h, 386, b"00000000000\0");
    put(&mut h, 398, b"00000001000\0");
    put(&mut h, 483, b"00000001000\0");
    checksum(&mut h);
    let mut v = Vec::new();
    v.extend_from_slice(&h);
    v.extend_from_slice(&padded(&vec![b'S'; 512]));
    v.extend_from_slice(&end_marker());
    sparse_cases.push(("gnu magic, one sparse block", v));

    let mut v = Vec::new();
    v.extend_from_slice(&raw_header("sparse.bin", b'S', "", 0, 0o644));
    v.extend_from_slice(&raw_header("after.txt", b'0', "", 2, 0o644));
    v.extend_from_slice(&padded(b"hi"));
    v.extend_from_slice(&end_marker());
    sparse_cases.push(("posix magic sparse typeflag", v));

    let mut v = Vec::new();
    v.extend_from_slice(&raw_header("good.txt", b'0', "", 4, 0o644));
    v.extend_from_slice(&padded(b"good"));
    v.extend_from_slice(&raw_header_magic(
        "sparse.bin",
        b'S',
        "",
        0,
        0o644,
        b"ustar ",
        b" \0",
    ));
    v.extend_from_slice(&end_marker());
    sparse_cases.push(("sparse entry after a good one", v));

    for (label, bytes) in sparse_cases {
        let mine = walk_mine(&bytes);
        assert!(
            mine.errored,
            "sparse: {label} — a sparse entry must be refused outright, got {:?}",
            mine.entries
        );
        feed_walk(&mine);
    }
    pin(SPARSE);
}

/// The header fields, RANDOMISED, with the checksum recomputed afterwards.
#[test]
fn checksum_repaired_header_mutations_are_pinned() {
    let mut rng = Rng(0x7A12_0005);
    for _ in 0..40_000 {
        let bytes = random_valid_checksum_archive(&mut rng);
        record(&bytes);
    }
    pin(CHECKSUM_REPAIRED_HEADER_MUTATIONS);
}

/// Real archives, mutated in a header, with that header's checksum REPAIRED.
#[test]
fn repaired_mutations_of_real_archives_are_pinned() {
    let sources = [ustar_archive(), handmade_archive()];
    let mut rng = Rng(0x7A12_0006);
    for source in &sources {
        for _ in 0..20_000 {
            let mut v = source.clone();
            let blocks = v.len() / 512;
            if blocks == 0 {
                continue;
            }
            let block = rng.below(blocks);
            let base = block * 512;
            for _ in 0..1 + rng.below(6) {
                let mut off = rng.below(504);
                if off >= 148 {
                    off += 8;
                }
                if let Some(slot) = v.get_mut(base + off) {
                    *slot = (rng.next_u64() & 0xff) as u8;
                }
            }
            let mut h = [0u8; 512];
            h.copy_from_slice(&v[base..base + 512]);
            checksum(&mut h);
            v[base..base + 512].copy_from_slice(&h);
            record(&v);
        }
    }
    pin(REPAIRED_MUTATIONS_OF_REAL_ARCHIVES);
}

/// One archive of one or two randomised-but-checksum-valid headers.
fn random_valid_checksum_archive(rng: &mut Rng) -> Vec<u8> {
    let mut v = Vec::new();
    for _ in 0..1 + rng.below(2) {
        let (h, body) = random_valid_header(rng);
        v.extend_from_slice(&h);
        v.extend_from_slice(&body);
    }
    if rng.below(4) != 0 {
        v.extend_from_slice(&end_marker());
    }
    v
}

/// One randomised header with a VALID checksum, plus a plausible body for it.
fn random_valid_header(rng: &mut Rng) -> ([u8; 512], Vec<u8>) {
    let mut h = [0u8; 512];

    let name: Vec<u8> = match rng.below(6) {
        0 => b"a.txt".to_vec(),
        1 => Vec::new(),
        2 => b"dir/".to_vec(),
        3 => b"././@LongLink".to_vec(),
        4 => vec![b'x'; 100],
        _ => (0..rng.below(20))
            .map(|_| (rng.next_u64() & 0xff) as u8)
            .collect(),
    };
    put(&mut h, 0, &name);
    let link: Vec<u8> = match rng.below(4) {
        0 => Vec::new(),
        1 => b"a.txt".to_vec(),
        2 => vec![b'l'; 100],
        _ => (0..rng.below(12))
            .map(|_| (rng.next_u64() & 0xff) as u8)
            .collect(),
    };
    put(&mut h, 157, &link);
    let prefix: Vec<u8> = match rng.below(4) {
        0 => Vec::new(),
        1 => b"pre/fix".to_vec(),
        2 => vec![b'p'; 155],
        _ => (0..rng.below(10))
            .map(|_| (rng.next_u64() & 0xff) as u8)
            .collect(),
    };
    put(&mut h, 345, &prefix);

    let numeric = |h: &mut [u8; 512], off: usize, len: usize, small: u64, rng: &mut Rng| {
        // Octal, base-256, or garbage.
        match rng.below(6) {
            0 => put(
                h,
                off,
                format!("{:0width$o}\0", small, width = len - 1).as_bytes(),
            ),
            1 => put(
                h,
                off,
                format!("{:0width$o} ", small, width = len - 1).as_bytes(),
            ),
            2 => {
                h[off..off + len].fill(0);
                h[off] = 0x80;
                let v = rng.next_u64() % 4096;
                h[off + len - 1] = (v & 0xff) as u8;
                h[off + len - 2] = ((v >> 8) & 0xff) as u8;
                if rng.below(2) == 0 {
                    h[off + 1] = (rng.next_u64() & 0xff) as u8;
                }
            }
            3 => put(h, off, b"garbage\0"),
            4 => {
                for slot in &mut h[off..off + len] {
                    *slot = (rng.next_u64() & 0xff) as u8;
                }
            }
            _ => put(
                h,
                off,
                format!("{:0width$o}\0", 0, width = len - 1).as_bytes(),
            ),
        }
    };
    numeric(&mut h, 100, 8, u64::from(rng.below(4096) as u32), rng);
    numeric(&mut h, 108, 8, 501, rng);
    numeric(&mut h, 116, 8, 20, rng);
    numeric(&mut h, 136, 12, 0, rng);

    let declared: u64 = match rng.below(8) {
        0 => 0,
        1 => 5,
        2 => 512,
        3 => 600,
        4 => u64::from(rng.below(40) as u32),
        5 => 0o77_777_777_777,
        6 => u64::MAX,
        _ => u64::from(rng.below(1500) as u32),
    };
    match rng.below(4) {
        0 => {
            h[124..136].fill(0);
            h[124] = 0x80;
            for (i, shift) in (0..8).enumerate() {
                h[135 - i] = ((declared >> (8 * shift)) & 0xff) as u8;
            }
        }
        1 => numeric(&mut h, 124, 12, declared, rng),
        _ => put(&mut h, 124, format!("{declared:011o}\0").as_bytes()),
    }

    const FLAGS: &[u8] = &[
        b'0', 0, b'1', b'2', b'5', b'7', b'6', b'3', b'L', b'K', b'x', b'g', b'S', b'V',
    ];
    h[156] = match rng.below(FLAGS.len() + 1) {
        i if i < FLAGS.len() => FLAGS[i],
        _ => (rng.next_u64() & 0xff) as u8,
    };

    let (magic, version): (&[u8], &[u8]) = match rng.below(6) {
        0 | 1 => (b"ustar\0", b"00"),
        2 => (b"ustar ", b" \0"),
        3 => (b"ustar\0", b" \0"),
        4 => (b"ustar ", b"00"),
        _ => (b"xxxxx\0", b"zz"),
    };
    put(&mut h, 257, magic);
    put(&mut h, 263, version);
    checksum(&mut h);

    let body_len = usize::try_from(declared.min(2048)).unwrap_or(0);
    let body: Vec<u8> = match h[156] {
        b'x' | b'g' => {
            let mut b = Vec::new();
            for _ in 0..1 + rng.below(3) {
                match rng.below(7) {
                    0 => b.extend_from_slice(&pax_record("path", "pax/name.txt")),
                    1 => b.extend_from_slice(&pax_record("linkpath", "pax/target")),
                    2 => b.extend_from_slice(&pax_record("size", "4")),
                    3 => b.extend_from_slice(&pax_record("size", "600")),
                    4 => b.extend_from_slice(b"999 path=x\n"),
                    5 => b.extend_from_slice(&pax_record("mtime", "1700000000")),
                    _ => b.push(b'\n'),
                }
            }
            b.resize(body_len, 0);
            b
        }
        b'L' | b'K' => {
            let mut b = b"gnu/long/name.txt".to_vec();
            b.push(0);
            b.resize(body_len, 0);
            b
        }
        _ => vec![b'D'; body_len],
    };
    (h, padded(&body))
}

/// Single-byte mutations of known-good archives — the corpus where a parser's
/// real disagreements live, because every mutation lands somewhere structural.
#[test]
fn byte_mutations_are_pinned() {
    let sources = [ustar_archive(), handmade_archive()];
    let mut rng = Rng(0x7A12_0001);
    let mut checked = 0usize;
    for source in &sources {
        for _ in 0..40_000 {
            let mut v = source.clone();
            let flips = 1 + rng.below(3);
            for _ in 0..flips {
                let at = rng.below(v.len());
                if let Some(slot) = v.get_mut(at) {
                    *slot = (rng.next_u64() & 0xff) as u8;
                }
            }
            record(&v);
            checked += 1;
        }
    }
    assert!(checked >= 80_000, "mutation corpus went missing");
    pin(BYTE_MUTATIONS);
}

/// Structured mutations: whole blocks zeroed, duplicated, dropped or swapped.
#[test]
fn block_mutations_are_pinned() {
    let sources = [ustar_archive(), handmade_archive()];
    let mut rng = Rng(0x7A12_0002);
    for source in &sources {
        let blocks: Vec<Vec<u8>> = source.chunks(512).map(<[u8]>::to_vec).collect();
        for _ in 0..12_000 {
            let mut b = blocks.clone();
            match rng.below(4) {
                0 if !b.is_empty() => {
                    let i = rng.below(b.len());
                    b[i] = vec![0u8; 512];
                }
                1 if !b.is_empty() => {
                    let i = rng.below(b.len());
                    let dup = b[i].clone();
                    b.insert(i, dup);
                }
                2 if !b.is_empty() => {
                    let i = rng.below(b.len());
                    b.remove(i);
                }
                _ if b.len() > 1 => {
                    let i = rng.below(b.len());
                    let j = rng.below(b.len());
                    b.swap(i, j);
                }
                _ => {}
            }
            record(&b.concat());
        }
    }
    pin(BLOCK_MUTATIONS);
}

/// Pure random bytes: refused, never a panic.
#[test]
fn random_bytes_are_pinned() {
    let mut rng = Rng(0x7A12_0003);
    for _ in 0..10_000 {
        let len = rng.below(3000);
        let mut v = vec![0u8; len];
        for slot in &mut v {
            *slot = (rng.next_u64() & 0xff) as u8;
        }
        record(&v);
    }
    pin(RANDOM_BYTES);
}

/// Reading a `.tar.zst` end to end through the same shape the extractor uses,
/// as a smoke test that nothing about the streaming (rather than in-memory)
/// case differs.
#[test]
fn streams_from_a_zstd_decoder_like_the_extractor() {
    let raw = handmade_archive();
    let mut compressed = Vec::new();
    aterm_zstd::frame::write_raw(&mut compressed, &raw, aterm_zstd::frame::Checksum::Xxh64)
        .expect("frame");
    let decoder = aterm_zstd::Decoder::new(&compressed[..]);

    use atpkg::tarread::Archive;
    let mut archive = Archive::new(decoder);
    let mut entries = archive.entries().expect("entries");
    let mut seen = Vec::new();
    while let Some(entry) = entries.next_entry().expect("walk") {
        seen.push(entry.path().expect("path").into_owned());
    }
    let in_memory = walk_mine(&raw);
    assert!(!in_memory.errored);
    let expected: Vec<PathBuf> = in_memory.entries.iter().map(|e| e.path.clone()).collect();
    assert_eq!(seen, expected, "streaming and in-memory walks must agree");
}

const EMPTY_AND_MARKER_ONLY_ARCHIVES: &str = "68e0b9cbef51eb61";
const ADVERSARIAL_HEADERS: &str = "75a8c972a8d47c1b";
const MALFORMED_PAX_RECORDS: &str = "dbbc4c862fd06887";
const MALFORMED_GNU_LONG_NAMES: &str = "dfcb97d991201c19";
const PAX_SIZE_RECORDS: &str = "c81421544fe5ae0b";
const PAX_GLOBAL_HEADERS: &str = "921b7363448ceb57";
const DUPLICATE_AND_RECOVERED_PAX_RECORDS: &str = "709decb1b388bf9a";
const MAGIC_AND_VERSION_COMBINATIONS: &str = if cfg!(windows) {
    "bee39615eac71f98"
} else {
    "8eefe78cdff18f7c"
};
const MAGICLESS_EXTENSION_HEADERS: &str = "932ba0fd24080695";
const DUPLICATE_EXTENSION_HEADERS: &str = "f5ba49d2eebd7a02";
const BASE256_NUMERIC_FIELDS: &str = "6cabebd3910a7260";
const CHECKSUM_REPAIRED_HEADER_MUTATIONS: &str = if cfg!(windows) {
    "851ffe23e6ca6ed2"
} else {
    "2f62c6b7a9a2b5e6"
};
const REPAIRED_MUTATIONS_OF_REAL_ARCHIVES: &str = if cfg!(windows) {
    "255d8b6a7a638e0f"
} else {
    "5a67be6fd371e14a"
};
const BYTE_MUTATIONS: &str = if cfg!(windows) {
    "2636f847ee73c9c3"
} else {
    "d8d32811cf9fff73"
};
const BLOCK_MUTATIONS: &str = "3778168542b363d6";
const RANDOM_BYTES: &str = "0467702b21fd1c25";
const SPARSE: &str = "beedb3b4d1868b3b";
const USTAR_ARCHIVE: &str = "01c3eddb42e31825";
const EVERY_ENTRY_TYPE: &str = "043e7dcfa9f08882";
const TRUNCATION: &str = "4443498b3936a499";
