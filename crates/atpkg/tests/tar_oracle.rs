// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! DIFFERENTIAL ORACLE for [`atpkg::tarread`], the first-party read-only tar
//! parser that retired the `tar` crate (and `xattr`, and `filetime`) from the
//! shipped graph.
//!
//! The retired crate is kept as a `[dev-dependencies]` entry for exactly this
//! file. A dev-dependency never enters the shipped graph, so the evidence is
//! free — and this is the one target where evidence is not optional: the bytes
//! being parsed are a DOWNLOADED PACKAGE BUNDLE, read before its signature has
//! bought anything.
//!
//! Two properties are asserted, over every input:
//!
//!   1. **Agreement on accept-or-reject.** If one reader walks an archive to
//!      the end, so does the other; if one refuses, so does the other.
//!   2. **Agreement on every field the extractor reads** — path, entry type,
//!      mode, declared size, link target, and the content bytes themselves —
//!      for every entry both accepted, in order.
//!
//! The corpus:
//!
//!   * archives built by the retired crate's own writer (ustar, long names,
//!     every entry type the extractor classifies);
//!   * archives written by the SYSTEM `tar` — bsdtar's restricted-pax and GNU
//!     tar's `L`/`K` long names are what real bundles actually contain, and
//!     neither is what a Rust writer emits;
//!   * hand-built adversarial headers: bad checksums, base-256 sizes, octal
//!     garbage, sizes that overflow, truncation at every block boundary,
//!     malformed PAX records, every magic/version combination crossed with a
//!     non-empty `prefix`, magic-less and duplicated extension headers, and
//!     the PAX `size` override in every shape;
//!   * tens of thousands of MUTATED archives from a fixed PRNG — single-byte
//!     flips, truncations and splices of the valid ones above, which is where
//!     a parser's real disagreements live;
//!   * tens of thousands of CHECKSUM-REPAIRED archives, which is where they
//!     actually live. See below.
//!
//! # Why the checksum has to be repaired
//!
//! A mutation corpus that flips bytes and leaves the checksum alone cannot say
//! anything about header FIELDS. Any flip that lands in a header invalidates
//! its checksum, both readers reject on the checksum, and the trial agrees for
//! a reason that has nothing to do with the field that changed. Measured on
//! this file's own corpus: of 40,000 mutations of `builder_archive()`, 31,618
//! had both readers error and 8,382 had both walk to the end (those are the
//! flips that landed in content or padding) — and ZERO disagreed, because the
//! accept/reject axis for header fields was never reached.
//!
//! So `checksum_repaired_header_mutations_match_the_oracle` and
//! `repaired_mutations_of_real_archives_match_the_oracle` choose the fields
//! FIRST and make the checksum valid afterwards. That is the space where two
//! tar parsers actually differ, and every one of these divergences was found
//! there rather than reasoned about: the PAX `size` override, the
//! typeflag-versus-magic asymmetry that suppresses it, the empty long-link
//! override that is `Some("")` and not `None`, and GNU sparse.
//!
//! # The one deliberate deviation
//!
//! GNU sparse (`S`) entries are REFUSED by the first-party reader rather than
//! parsed. `gnu_sparse_entries_are_refused_by_both_readers` states and pins the
//! weaker property that holds there — no archive with a sparse entry installs
//! under either reader — instead of pretending to an agreement that does not
//! exist. Everything else in this file is exact agreement.

use std::io::{Read, Write};
use std::path::PathBuf;

/// One entry, reduced to exactly what `atpkg::extract` reads out of it.
#[derive(Debug, PartialEq, Eq)]
struct Record {
    path: PathBuf,
    kind: &'static str,
    mode: u32,
    /// The EFFECTIVE content length — after a PAX `size` record has overridden
    /// the header field. This is the number that decides where the next header
    /// begins, so a disagreement here is a disagreement about the whole rest of
    /// the archive.
    size: u64,
    /// The RAW `size` field, unoverridden. Compared separately so a divergence
    /// in the override and a divergence in the field parse cannot cancel.
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

/// Walk with the RETIRED crate. Deliberately a separate function rather than a
/// generic one: the two must not be able to share a bug.
fn walk_oracle(bytes: &[u8]) -> Walk {
    let mut archive = tar::Archive::new(bytes);
    let mut entries = Vec::new();
    let src = match archive.entries() {
        Ok(e) => e,
        Err(_) => {
            return Walk {
                entries,
                errored: true,
            };
        }
    };
    for next in src {
        let Ok(mut entry) = next else {
            return Walk {
                entries,
                errored: true,
            };
        };
        let kind = match entry.header().entry_type() {
            tar::EntryType::Regular => "regular",
            tar::EntryType::Continuous => "continuous",
            tar::EntryType::Directory => "directory",
            tar::EntryType::Symlink => "symlink",
            tar::EntryType::Link => "link",
            _ => "other",
        };
        let size = entry.size();
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
            // The retired crate does not mask the mode field; the first-party
            // reader keeps only the 12 permission bits, because that is all
            // `safe_mode` can act on and a wider value is noise from a hostile
            // writer. Compare the part both agree describes permissions.
            mode: mode & 0o7777,
            size,
            header_size,
            link,
            content,
        });
    }
    Walk {
        entries,
        errored: false,
    }
}

/// Hex dump for a failure message, so a disagreement is reproducible from the
/// transcript alone.
fn dump(bytes: &[u8]) -> String {
    let mut out = String::new();
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Assert the two readers agree about `bytes`.
#[track_caller]
fn agree(bytes: &[u8], what: &str) {
    let mine = walk_mine(bytes);
    let theirs = walk_oracle(bytes);
    assert_eq!(
        mine.errored,
        theirs.errored,
        "{what}: accept/reject disagreement — first-party errored={}, oracle errored={}\n\
         first-party entries: {:?}\noracle entries: {:?}\nINPUT: {}",
        mine.errored,
        theirs.errored,
        mine.entries,
        theirs.entries,
        dump(bytes)
    );
    assert_eq!(
        mine.entries.len(),
        theirs.entries.len(),
        "{what}: entry-count disagreement\nfirst-party: {:?}\noracle: {:?}",
        mine.entries,
        theirs.entries
    );
    for (i, (a, b)) in mine.entries.iter().zip(&theirs.entries).enumerate() {
        assert_eq!(
            a,
            b,
            "{what}: entry {i} disagreement\nINPUT: {}",
            dump(bytes)
        );
    }
}

// ---------------------------------------------------------------------------
// Corpus builders
// ---------------------------------------------------------------------------

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

/// [`raw_header`] with the magic and version chosen by the caller.
///
/// The two fields together are what separates POSIX ustar (`"ustar\0"` + `"00"`)
/// from GNU (`"ustar "` + `" \0"`) from something that is neither — and every
/// fixture in this file used to write POSIX, which is precisely why the
/// magic-blind bugs this now covers were invisible.
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

/// A small archive written by the retired crate's own writer — the "known
/// good" shape every mutation starts from.
fn builder_archive() -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    for (name, body, exec) in [
        ("bin/aterm", &b"ELF-ish payload"[..], true),
        ("share/doc/readme.txt", &b"hello"[..], false),
        ("empty.txt", &b""[..], false),
        (
            "a/very/deeply/nested/path/that/goes/on/and/on/and/on/for/rather/a/long/way/indeed/so/that/the/hundred/byte/name/field/cannot/hold/it.txt",
            &b"long name"[..],
            false,
        ),
    ] {
        let mut h = tar::Header::new_ustar();
        h.set_size(body.len() as u64);
        h.set_mode(if exec { 0o755 } else { 0o644 });
        h.set_mtime(0);
        h.set_entry_type(tar::EntryType::Regular);
        h.set_cksum();
        b.append_data(&mut h, name, body).expect("append");
    }
    let mut h = tar::Header::new_ustar();
    h.set_size(0);
    h.set_mode(0o755);
    h.set_mtime(0);
    h.set_entry_type(tar::EntryType::Directory);
    h.set_cksum();
    b.append_data(&mut h, "share/doc/", std::io::empty())
        .expect("append dir");
    b.into_inner().expect("finish").to_vec()
}

/// A hand-built archive exercising every entry type the extractor classifies,
/// plus a GNU long name and a PAX `path` record — the two extension forms real
/// system tars emit.
fn handmade_archive() -> Vec<u8> {
    let mut v = Vec::new();
    // Plain file.
    v.extend_from_slice(&raw_header("plain.txt", b'0', "", 5, 0o644));
    v.extend_from_slice(&padded(b"12345"));
    // Directory.
    v.extend_from_slice(&raw_header("dir/", b'5', "", 0, 0o755));
    // Symlink.
    v.extend_from_slice(&raw_header("link", b'2', "plain.txt", 0, 0o777));
    // Hard link.
    v.extend_from_slice(&raw_header("hard", b'1', "plain.txt", 0, 0o644));
    // Continuation type — treated as a regular file by both readers.
    v.extend_from_slice(&raw_header("cont.txt", b'7', "", 3, 0o644));
    v.extend_from_slice(&padded(b"abc"));
    // A FIFO: neither reader has an opinion beyond "other".
    v.extend_from_slice(&raw_header("fifo", b'6', "", 0, 0o644));
    // GNU long name, then the entry it names.
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
    // PAX extended header carrying a path, then its entry.
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
    // The length counts its own digits, so it is a fixed point: grow until the
    // written length matches.
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn writer_built_archive_matches_the_oracle() {
    agree(&builder_archive(), "builder archive");
}

#[test]
fn every_entry_type_and_extension_matches_the_oracle() {
    agree(&handmade_archive(), "handmade archive");
}

#[test]
fn empty_and_marker_only_archives_match_the_oracle() {
    agree(&[], "empty input");
    agree(&end_marker(), "marker only");
    agree(&vec![0u8; 512], "one zero block");
    agree(&vec![0u8; 511], "half a zero block");
    agree(&vec![0u8; 2048], "four zero blocks");
}

#[test]
fn truncation_at_every_boundary_matches_the_oracle() {
    for source in [builder_archive(), handmade_archive()] {
        // Every block boundary, plus a handful of ragged offsets inside the
        // first few blocks where the header itself is cut in half.
        let mut cuts: Vec<usize> = (0..=source.len()).step_by(512).collect();
        cuts.extend([1, 7, 100, 148, 156, 257, 511, 513, 700, 1023]);
        for cut in cuts {
            if cut > source.len() {
                continue;
            }
            agree(&source[..cut], &format!("truncated at {cut}"));
        }
    }
}

#[test]
fn adversarial_headers_match_the_oracle() {
    // A header whose checksum is wrong by one.
    let mut bad_sum = raw_header("x", b'0', "", 0, 0o644);
    bad_sum[148] = b'1';
    agree(&[&bad_sum[..], &end_marker()].concat(), "bad checksum");

    // Non-octal garbage in the size field.
    let mut bad_size = raw_header("x", b'0', "", 0, 0o644);
    bad_size[124..136].copy_from_slice(b"9999999999\0\0");
    checksum(&mut bad_size);
    agree(&[&bad_size[..], &end_marker()].concat(), "non-octal size");

    // A size field that overflows u64 in octal (24 octal digits would, but the
    // field is 12 — so use the base-256 form, which can).
    let mut b256 = raw_header("x", b'0', "", 0, 0o644);
    b256[124] = 0x80 | 0x7f;
    b256[125..136].fill(0xff);
    checksum(&mut b256);
    agree(&[&b256[..], &end_marker()].concat(), "base-256 overflow");

    // A legitimate base-256 size, small enough to be real.
    let mut b256_ok = raw_header("x", b'0', "", 0, 0o644);
    b256_ok[124] = 0x80;
    b256_ok[125..135].fill(0);
    b256_ok[135] = 4;
    checksum(&mut b256_ok);
    agree(
        &[&b256_ok[..], &padded(b"data"), &end_marker()].concat(),
        "base-256 size",
    );

    // An enormous declared size with no body behind it.
    let mut huge = raw_header("x", b'0', "", 0, 0o644);
    huge[124..136].copy_from_slice(b"77777777777\0");
    checksum(&mut huge);
    agree(&[&huge[..], &end_marker()].concat(), "huge declared size");

    // A ustar prefix that must be joined to the name with a slash.
    let mut prefixed = raw_header("tail.txt", b'0', "", 0, 0o644);
    let pre = b"some/long/prefix/path";
    prefixed[345..345 + pre.len()].copy_from_slice(pre);
    checksum(&mut prefixed);
    agree(&[&prefixed[..], &end_marker()].concat(), "ustar prefix");

    // An empty name.
    let empty_name = raw_header("", b'0', "", 0, 0o644);
    agree(&[&empty_name[..], &end_marker()].concat(), "empty name");
}

#[test]
fn malformed_pax_records_match_the_oracle() {
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
    for (label, body) in cases {
        let mut v = Vec::new();
        v.extend_from_slice(&raw_header("PaxHeader", b'x', "", body.len() as u64, 0o644));
        v.extend_from_slice(&padded(&body));
        v.extend_from_slice(&raw_header("real.txt", b'0', "", 2, 0o644));
        v.extend_from_slice(&padded(b"hi"));
        v.extend_from_slice(&end_marker());
        agree(&v, &format!("pax: {label}"));
    }
}

#[test]
fn malformed_gnu_long_names_match_the_oracle() {
    // A long-name header with no entry after it.
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
    agree(&v, "dangling long name");

    // A long LINK name applied to a symlink.
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
    agree(&v, "long link name");

    // A zero-length long name.
    let mut v = Vec::new();
    v.extend_from_slice(&raw_header("././@LongLink", b'L', "", 0, 0o644));
    v.extend_from_slice(&raw_header("fallback.txt", b'0', "", 0, 0o644));
    v.extend_from_slice(&end_marker());
    agree(&v, "zero-length long name");
}

/// A PAX `size` record OVERRIDES the header's `size` field — the whole
/// file-list-substitution vector on the bundle path.
///
/// An `x` header saying `size=4` in front of a ustar header saying `size=512`
/// makes the two readers disagree about where the NEXT header starts, and from
/// there about every entry in the rest of the archive. The retired crate carries
/// the reason in its own source: "Disagreement among parsers allows construction
/// of malicious archives that appear different when parsed."
#[test]
fn pax_size_records_match_the_oracle() {
    // A PAX size SMALLER than the header's, with the header's declared bytes
    // present. A reader that ignores the record reads 512 content bytes; a
    // reader that honours it reads 4, and the other 508 become the next header.
    let mut v = Vec::new();
    let rec = pax_record("size", "4");
    v.extend_from_slice(&raw_header("PaxHeader", b'x', "", rec.len() as u64, 0o644));
    v.extend_from_slice(&padded(&rec));
    v.extend_from_slice(&raw_header("f.txt", b'0', "", 512, 0o644));
    v.extend_from_slice(&padded(&vec![b'Z'; 512]));
    v.extend_from_slice(&end_marker());
    agree(&v, "pax size smaller than the header size");

    // …and the same shape with a SECOND real entry behind it, so a divergence
    // in where the next header starts shows up as a different entry list rather
    // than only a different content length.
    let mut v = Vec::new();
    let rec = pax_record("size", "4");
    v.extend_from_slice(&raw_header("PaxHeader", b'x', "", rec.len() as u64, 0o644));
    v.extend_from_slice(&padded(&rec));
    v.extend_from_slice(&raw_header("f.txt", b'0', "", 512, 0o644));
    v.extend_from_slice(&padded(&vec![b'Z'; 512]));
    v.extend_from_slice(&raw_header("g.txt", b'0', "", 3, 0o644));
    v.extend_from_slice(&padded(b"xyz"));
    v.extend_from_slice(&end_marker());
    agree(&v, "pax size smaller, two entries");

    // A PAX size LARGER than the header's.
    let mut v = Vec::new();
    let rec = pax_record("size", "600");
    v.extend_from_slice(&raw_header("PaxHeader", b'x', "", rec.len() as u64, 0o644));
    v.extend_from_slice(&padded(&rec));
    v.extend_from_slice(&raw_header("big.txt", b'0', "", 4, 0o644));
    v.extend_from_slice(&padded(&vec![b'Q'; 600]));
    v.extend_from_slice(&end_marker());
    agree(&v, "pax size larger than the header size");

    // A PAX size on a DIRECTORY — the entry kind `extract.rs` guards with
    // `declared != 0`, so the two readers must agree on which number that is.
    for (label, declared, pax_size) in [
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
        agree(&v, label);
    }

    // A PAX size in front of a GLOBAL header. An extension header's own body
    // length is its own business, so the override must NOT apply there.
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
    agree(&v, "pax size in front of a global header");

    // Values a `size` record has no business carrying.
    for (label, value) in [
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
        agree(&v, &format!("pax size value: {label}"));
    }

    // `size` alongside `path` and `linkpath`, in both orders — the combination
    // a real bsdtar archive emits for a long-named large file.
    for (label, body) in [
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
        agree(&v, &format!("pax combination: {label}"));
    }
}

/// A PAX GLOBAL (`g`) header is an ORDINARY entry, not an extension header.
///
/// It must never supply a `path` to the entry after it: a global header is a
/// default for every following member, and honouring one as a rename primitive
/// hands a crafted archive a name override that `vet_entry` has never been
/// exercised against. The retired reader yielded the `g` block as an entry,
/// which `classify` maps to `Other` and the extractor refuses.
#[test]
fn pax_global_headers_match_the_oracle() {
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("global path", pax_record("path", "global/override.txt")),
        ("global linkpath", pax_record("linkpath", "global/target")),
        ("global size", pax_record("size", "4")),
        ("global empty body", Vec::new()),
        ("global malformed", b"999 path=x\n".to_vec()),
    ];
    for (label, body) in cases {
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
        agree(&v, &format!("pax global: {label}"));
    }

    // A global header as the LAST thing in the archive — nothing for a reader
    // that thinks it is an extension header to attach it to.
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
    agree(&v, "global header at the end of the archive");

    // A global header with a GNU magic rather than a POSIX one.
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
    agree(&v, "gnu-magic global header");
}

/// Duplicate PAX keys resolve to the FIRST record, and a malformed record does
/// not end the scan.
///
/// Both are cases where "obviously the last one wins" and "obviously junk is
/// fatal" are each defensible and each WRONG, because being the only reader
/// that picks them is what lets one archive show two readers two names.
#[test]
fn duplicate_and_recovered_pax_records_match_the_oracle() {
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
    for (label, body) in cases {
        let mut v = Vec::new();
        v.extend_from_slice(&raw_header("PaxHeader", b'x', "", body.len() as u64, 0o644));
        v.extend_from_slice(&padded(&body));
        v.extend_from_slice(&raw_header("hdr.txt", b'2', "hdr-target", 0, 0o777));
        v.extend_from_slice(&end_marker());
        agree(&v, &format!("pax records: {label}"));
    }
}

/// `prefix` is a POSIX-ustar field and nothing else.
///
/// Bytes 345..500 are `prefix` on a POSIX header and `atime`/`ctime`/`offset` on
/// a GNU one, so a reader that keys on `magic.starts_with("ustar")` reads a GNU
/// header's timestamps as a directory component and invents a path no other
/// reader produces.
#[test]
fn magic_and_version_combinations_match_the_oracle() {
    let magics: [(&str, &[u8], &[u8]); 6] = [
        ("posix", b"ustar\0", b"00"),
        ("gnu", b"ustar ", b" \0"),
        ("posix magic, gnu version", b"ustar\0", b" \0"),
        ("gnu magic, posix version", b"ustar ", b"00"),
        ("garbage magic", b"xxxxx\0", b"00"),
        ("no magic", b"\0\0\0\0\0\0", b"\0\0"),
    ];
    for (label, magic, version) in magics {
        for (prefix_label, prefix) in [
            ("empty prefix", &b""[..]),
            ("path-shaped prefix", &b"EVIL"[..]),
            ("long prefix", &b"some/long/prefix/path"[..]),
            ("binary prefix", &[0x80, 0x01, 0x02, 0x03][..]),
        ] {
            let mut h = raw_header_magic("tail.txt", b'0', "", 0, 0o644, magic, version);
            put(&mut h, 345, prefix);
            checksum(&mut h);
            agree(
                &[&h[..], &end_marker()].concat(),
                &format!("magic {label} / {prefix_label}"),
            );
        }
    }
}

/// `L`/`K`/`x` are extension typeflags only on a header that speaks ustar or
/// GNU. With neither magic the block is an ordinary entry — which the extractor
/// then refuses as a disallowed kind.
#[test]
fn magicless_extension_headers_match_the_oracle() {
    for flag in [b'L', b'K', b'x', b'g'] {
        for (label, magic, version) in [
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
            agree(&v, &format!("typeflag {} with {label}", flag as char));
        }
    }
}

/// A SECOND `L`, `K` or `x` before the same member is two answers to "what is
/// this file called", and last-wins would make the answer depend on which
/// parser you asked.
#[test]
fn duplicate_extension_headers_match_the_oracle() {
    let long_name = |s: &str| {
        let mut b = s.as_bytes().to_vec();
        b.push(0);
        b
    };
    let cases: Vec<(&str, Vec<(u8, Vec<u8>)>)> = vec![
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
    for (label, headers) in cases {
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
        agree(&v, &format!("duplicate extensions: {label}"));
    }
}

/// The GNU base-256 numeric extension belongs to the fields that DEFINE it.
///
/// `chksum` and `mode` are octal-only: a high-bit checksum field lets a header
/// carry a base-256 checksum equal to its own byte sum, and a high-bit mode
/// field turns "unreadable, fall back to 0o644" into "attacker-chosen, and the
/// `0o111` bits drive `safe_mode` to 0o755".
#[test]
fn base256_numeric_fields_match_the_oracle() {
    // Base-256 in the CHECKSUM field, numerically correct for the header.
    let mut h = raw_header("x.txt", b'0', "", 0, 0o644);
    h[148..156].fill(b' ');
    let sum: u64 = h.iter().map(|&b| u64::from(b)).sum();
    h[148] = 0x80;
    for (i, shift) in (0..7).enumerate() {
        h[155 - i] = ((sum >> (8 * shift)) & 0xff) as u8;
    }
    agree(
        &[&h[..], &end_marker()].concat(),
        "base-256 checksum equal to the byte sum",
    );

    // Base-256 in the MODE field.
    let mut h = raw_header("x.txt", b'0', "", 0, 0o644);
    h[100] = 0x80;
    h[101..106].fill(0);
    h[106] = 0o1;
    h[107] = 0o355;
    checksum(&mut h);
    agree(&[&h[..], &end_marker()].concat(), "base-256 mode");

    // A 12-byte base-256 size whose LEADING bytes are non-zero. Only the last 8
    // are read, so this is size 4 — not an overflow.
    let mut h = raw_header("x.txt", b'0', "", 0, 0o644);
    h[124] = 0x80;
    h[125] = 0x01;
    h[126..135].fill(0);
    h[135] = 4;
    checksum(&mut h);
    agree(
        &[&h[..], &padded(b"data"), &end_marker()].concat(),
        "base-256 size with leading garbage above the low 8 bytes",
    );

    // The same shape with an all-zero low 8 bytes: size ZERO despite a set
    // leading byte.
    let mut h = raw_header("x.txt", b'0', "", 0, 0o644);
    h[124] = 0x80;
    h[125] = 0x7f;
    h[126] = 0xff;
    h[127] = 0xff;
    h[128..136].fill(0);
    checksum(&mut h);
    agree(
        &[&h[..], &end_marker()].concat(),
        "base-256 size whose low 8 bytes are zero",
    );

    // Base-256 in every other numeric field, one at a time — uid, gid, mtime.
    for (label, off, len) in [
        ("uid", 108usize, 8usize),
        ("gid", 116, 8),
        ("mtime", 136, 12),
    ] {
        let mut h = raw_header("x.txt", b'0', "", 0, 0o644);
        h[off..off + len].fill(0);
        h[off] = 0x80;
        h[off + len - 1] = 7;
        checksum(&mut h);
        agree(
            &[&h[..], &end_marker()].concat(),
            &format!("base-256 {label}"),
        );
    }

    // A base-256 size that saturates all 8 read bytes: u64::MAX, which the
    // block rounding cannot advance past.
    let mut h = raw_header("x.txt", b'0', "", 0, 0o644);
    h[124] = 0x80;
    h[125..136].fill(0xff);
    checksum(&mut h);
    agree(
        &[&h[..], &end_marker()].concat(),
        "base-256 size of u64::MAX",
    );
}

/// GNU SPARSE (`S`) entries: both readers refuse every archive carrying one.
///
/// This is the ONE place the first-party reader is deliberately stricter than
/// the retired one, so it gets its own test rather than an `agree` call. The
/// retired reader parsed the sparse block map; this one refuses the typeflag
/// outright, because a sparse entry classifies as `Other` and `vet_entry`
/// aborts the whole staged group on an `Other` entry anyway — so implementing
/// a second content-assembly path would buy nothing but a second place for two
/// readers to disagree about a file's bytes.
///
/// What must hold is the property that actually matters: NO archive containing
/// a sparse entry installs under either reader. The first-party reader errors;
/// the retired one either errors too or yields an entry the extractor refuses.
#[test]
fn gnu_sparse_entries_are_refused_by_both_readers() {
    let mut sparse_cases: Vec<(&str, Vec<u8>)> = Vec::new();

    // A GNU-magic sparse header with an all-zero sparse map.
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

    // The same with a plausible sparse map and realsize written in.
    let mut h = raw_header_magic("sparse.bin", b'S', "", 512, 0o644, b"ustar ", b" \0");
    // sparse[0] = (offset 0, numbytes 512) at 386; realsize at 483.
    put(&mut h, 386, b"00000000000\0");
    put(&mut h, 398, b"00000001000\0");
    put(&mut h, 483, b"00000001000\0");
    checksum(&mut h);
    let mut v = Vec::new();
    v.extend_from_slice(&h);
    v.extend_from_slice(&padded(&vec![b'S'; 512]));
    v.extend_from_slice(&end_marker());
    sparse_cases.push(("gnu magic, one sparse block", v));

    // A sparse typeflag on a POSIX ustar header — not a GNU header at all.
    let mut v = Vec::new();
    v.extend_from_slice(&raw_header("sparse.bin", b'S', "", 0, 0o644));
    v.extend_from_slice(&raw_header("after.txt", b'0', "", 2, 0o644));
    v.extend_from_slice(&padded(b"hi"));
    v.extend_from_slice(&end_marker());
    sparse_cases.push(("posix magic sparse typeflag", v));

    // A sparse entry AFTER a legitimate one, so the refusal is not merely
    // "the archive was broken from byte zero".
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
            "sparse: {label} — the first-party reader must refuse a sparse \
             entry outright, got {:?}",
            mine.entries
        );
        let theirs = walk_oracle(&bytes);
        let oracle_refuses = theirs.errored || theirs.entries.iter().any(|e| e.kind == "other");
        assert!(
            oracle_refuses,
            "sparse: {label} — the retired reader accepted this archive with no \
             entry the extractor would refuse, so the strictness here would be a \
             real behavioural divergence rather than a conservative one: {:?}",
            theirs.entries
        );
        // And everything the first-party reader DID yield before refusing must
        // be exactly what the retired one yielded for the same prefix — the
        // strictness may only cut the walk shorter, never describe the entries
        // before the cut differently.
        assert!(
            mine.entries.len() <= theirs.entries.len(),
            "sparse: {label} — the stricter reader yielded MORE entries than \
             the one it is stricter than"
        );
        for (i, (a, b)) in mine.entries.iter().zip(&theirs.entries).enumerate() {
            assert_eq!(a, b, "sparse: {label} — entry {i} before the refusal");
        }
    }
}

/// The header fields, RANDOMISED, with the checksum recomputed afterwards.
///
/// This is the corpus the byte/block mutators structurally cannot reach. They
/// flip bytes without repairing the checksum, so any mutation landing in a
/// header makes BOTH readers reject on the checksum and the comparison carries
/// no information about field semantics at all — 40,000 mutations of
/// `builder_archive()` produce exactly zero disagreements not because the
/// parsers agree about fields but because they never get as far as the fields.
///
/// Here the checksum is made VALID after the fields are chosen, so every trial
/// lands in valid-checksum/hostile-field space: magic and version combinations,
/// every typeflag, base-256 in every numeric field, prefixes, and PAX bodies.
#[test]
fn checksum_repaired_header_mutations_match_the_oracle() {
    let mut rng = Rng(0x7A12_0005);
    for _ in 0..40_000 {
        let bytes = random_valid_checksum_archive(&mut rng);
        agree(&bytes, "checksum-repaired header mutation");
    }
}

/// Real archives, mutated in a header, with that header's checksum REPAIRED.
///
/// The same idea as [`checksum_repaired_header_mutations_match_the_oracle`] but
/// starting from archives a real writer produced, so the mutated field sits in
/// an otherwise coherent multi-entry structure.
#[test]
fn repaired_mutations_of_real_archives_match_the_oracle() {
    let sources = [builder_archive(), handmade_archive()];
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
                // Anywhere in the header EXCEPT its own checksum field, which
                // is about to be rewritten.
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
            agree(&v, "repaired mutation of a real archive");
        }
    }
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

    // name / linkname / prefix: real-ish, empty, oversized, or binary.
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

    // Numeric fields: octal, base-256, or garbage — independently per field.
    let numeric = |h: &mut [u8; 512], off: usize, len: usize, small: u64, rng: &mut Rng| {
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
                // base-256
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
    numeric(&mut h, 100, 8, u64::from(rng.below(4096) as u32), rng); // mode
    numeric(&mut h, 108, 8, 501, rng); // uid
    numeric(&mut h, 116, 8, 20, rng); // gid
    numeric(&mut h, 136, 12, 0, rng); // mtime

    // Size: overwhelmingly small so the body is buildable, occasionally absurd.
    let declared: u64 = match rng.below(8) {
        0 => 0,
        1 => 5,
        2 => 512,
        3 => 600,
        4 => u64::from(rng.below(40) as u32),
        5 => 0o7777_7777_777,
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

    // Typeflag: every one either reader distinguishes, plus noise.
    const FLAGS: &[u8] = &[
        b'0', 0, b'1', b'2', b'5', b'7', b'6', b'3', b'L', b'K', b'x', b'g', b'S', b'V',
    ];
    h[156] = match rng.below(FLAGS.len() + 1) {
        i if i < FLAGS.len() => FLAGS[i],
        _ => (rng.next_u64() & 0xff) as u8,
    };

    // Magic / version: the two real combinations, the two crossed ones, and
    // neither.
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

    // A body: for an extension typeflag, a PAX-ish or long-name-ish one.
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
fn byte_mutations_match_the_oracle() {
    let sources = [builder_archive(), handmade_archive()];
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
            agree(&v, "byte mutation");
            checked += 1;
        }
    }
    assert!(checked >= 80_000, "mutation corpus went missing");
}

/// Structured mutations: whole blocks zeroed, duplicated, dropped or swapped.
#[test]
fn block_mutations_match_the_oracle() {
    let sources = [builder_archive(), handmade_archive()];
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
            agree(&b.concat(), "block mutation");
        }
    }
}

/// Pure random bytes: both readers must refuse, and neither may panic.
#[test]
fn random_bytes_match_the_oracle() {
    let mut rng = Rng(0x7A12_0003);
    for _ in 0..10_000 {
        let len = rng.below(3000);
        let mut v = vec![0u8; len];
        for slot in &mut v {
            *slot = (rng.next_u64() & 0xff) as u8;
        }
        agree(&v, "random bytes");
    }
}

/// Archives written by the SYSTEM tar — bsdtar's restricted-pax and GNU tar's
/// `L`/`K` long names are what a real bundle contains, and neither is what a
/// Rust writer emits. Skipped (not failed) where no `tar` binary exists.
#[test]
fn system_tar_archives_match_the_oracle() {
    let dir = std::env::temp_dir().join(format!("atpkg-tar-oracle-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let root = dir.join("root");
    std::fs::create_dir_all(root.join("share/doc")).expect("mkdir");
    std::fs::create_dir_all(root.join("bin")).expect("mkdir");
    std::fs::write(root.join("bin/aterm"), b"payload").expect("write");
    std::fs::write(root.join("share/doc/readme.txt"), b"hello world").expect("write");
    std::fs::write(root.join("empty"), b"").expect("write");
    // A name too long for the 100-byte field, so the writer MUST reach for its
    // long-name extension — the whole point of this case.
    let long_dir = root.join("l".repeat(60)).join("l".repeat(60));
    std::fs::create_dir_all(&long_dir).expect("mkdir long");
    std::fs::write(long_dir.join("deep.txt"), b"deep").expect("write long");
    #[cfg(unix)]
    std::os::unix::fs::symlink("bin/aterm", root.join("alias")).expect("symlink");

    let out = dir.join("out.tar");
    let status = std::process::Command::new("tar")
        .arg("-cf")
        .arg(&out)
        .arg("-C")
        .arg(&root)
        .arg(".")
        .status();
    match status {
        Ok(s) if s.success() => {}
        _ => {
            eprintln!("system_tar_archives_match_the_oracle: no usable `tar` binary — skipped");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
    }
    let bytes = std::fs::read(&out).expect("read archive");
    assert!(bytes.len() > 1024, "system tar produced nothing");
    agree(&bytes, "system tar default format");

    // ...and the same tree truncated at every block boundary, because a
    // partially-downloaded bundle is a real failure mode.
    for cut in (0..bytes.len()).step_by(512) {
        agree(&bytes[..cut], &format!("system tar truncated at {cut}"));
    }

    // Explicit formats where the installed tar supports them, so both the PAX
    // and the GNU long-name encodings are covered wherever they exist.
    //
    // `gnutar` AND `gnu`: bsdtar (macOS, and the tar this module's docs name as
    // the publisher's writer) calls the format `gnutar` and fails outright on
    // `gnu`; GNU tar calls it `gnu`. Naming only one of them is how the GNU
    // `L`/`K` lane and its whole mutation sub-corpus silently skipped on the
    // machine this parser was written on — the loop `continue`d and said
    // nothing. `ran` below is the fix for the silence: at least two explicit
    // formats must actually have produced an archive.
    let mut ran: Vec<&str> = Vec::new();
    for format in ["ustar", "pax", "gnutar", "gnu"] {
        let out = dir.join(format!("out-{format}.tar"));
        let ok = std::process::Command::new("tar")
            .arg("--format")
            .arg(format)
            .arg("-cf")
            .arg(&out)
            .arg("-C")
            .arg(&root)
            .arg(".")
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            continue;
        }
        let Ok(bytes) = std::fs::read(&out) else {
            continue;
        };
        ran.push(format);
        agree(&bytes, &format!("system tar --format {format}"));
        let mut rng = Rng(0x7A12_0004);
        for _ in 0..8_000 {
            let mut v = bytes.clone();
            let at = rng.below(v.len());
            if let Some(slot) = v.get_mut(at) {
                *slot = (rng.next_u64() & 0xff) as u8;
            }
            agree(&v, &format!("mutated system tar --format {format}"));
        }
    }
    // A GNU-flavoured format must be among them: `L`/`K` long-name blocks are
    // the encoding a Linux-built bundle carries, and they exist in no other
    // corpus here except the hand-built fixtures.
    assert!(
        ran.len() >= 2,
        "only {ran:?} of the explicit tar formats produced an archive — the \
         format names are wrong for the installed tar, and the corpus this \
         test claims to cover was never built"
    );
    assert!(
        ran.contains(&"gnutar") || ran.contains(&"gnu"),
        "no GNU-format archive was produced ({ran:?}); the `L`/`K` long-name \
         lane is the one real bundles from Linux use"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The reader must hand out exactly the declared content and never read past
/// it into the next header — the property that keeps a lying `size` from
/// letting one entry consume another.
#[test]
fn content_never_overruns_into_the_next_header() {
    let mut v = Vec::new();
    v.extend_from_slice(&raw_header("a.txt", b'0', "", 4, 0o644));
    v.extend_from_slice(&padded(b"AAAA"));
    v.extend_from_slice(&raw_header("b.txt", b'0', "", 4, 0o644));
    v.extend_from_slice(&padded(b"BBBB"));
    v.extend_from_slice(&end_marker());
    let walk = walk_mine(&v);
    assert!(!walk.errored);
    assert_eq!(walk.entries.len(), 2);
    assert_eq!(walk.entries[0].content, b"AAAA");
    assert_eq!(walk.entries[1].content, b"BBBB");
    agree(&v, "two entries");
}

/// An entry whose content the caller never reads must still be skipped
/// correctly — the extractor does exactly this for directories and hard links.
#[test]
fn unread_content_is_skipped() {
    let mut v = Vec::new();
    v.extend_from_slice(&raw_header("skipme.bin", b'0', "", 1000, 0o644));
    v.extend_from_slice(&padded(&vec![7u8; 1000]));
    v.extend_from_slice(&raw_header("after.txt", b'0', "", 5, 0o644));
    v.extend_from_slice(&padded(b"after"));
    v.extend_from_slice(&end_marker());

    use atpkg::tarread::Archive;
    let mut archive = Archive::new(&v[..]);
    let mut entries = archive.entries().expect("entries");
    let first = entries.next_entry().expect("ok").expect("some");
    assert_eq!(first.path().expect("path").to_str(), Some("skipme.bin"));
    drop(first);
    let mut second = entries.next_entry().expect("ok").expect("some");
    assert_eq!(second.path().expect("path").to_str(), Some("after.txt"));
    let mut body = Vec::new();
    second.read_to_end(&mut body).expect("read");
    assert_eq!(body, b"after");
}

/// Reading a `.tar.zst` end to end through the same shape the extractor uses,
/// as a smoke test that nothing about the streaming (rather than in-memory)
/// case differs.
#[test]
fn streams_from_a_zstd_decoder_like_the_extractor() {
    let raw = handmade_archive();
    let mut enc = zstd::Encoder::new(Vec::new(), 0).expect("encoder");
    enc.write_all(&raw).expect("write");
    let compressed = enc.finish().expect("finish");
    let decoder = zstd::Decoder::new(&compressed[..]).expect("decoder");

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
