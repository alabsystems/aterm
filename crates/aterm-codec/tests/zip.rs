// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `zip::Archive` against archives built here: stored and deflated members, the ZIP64
//! encoding, and the malformations the reader refuses.

use std::io::{Cursor, Read};

use aterm_codec::zip::{Archive, Entry};

struct Member<'a> {
    name: &'a str,
    mode: u32,
    data: &'a [u8],
    deflate: bool,
}

fn le16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn le32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn le64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// `data` as one final stored DEFLATE block (BTYPE 00): a valid raw deflate stream.
fn stored_deflate(data: &[u8]) -> Vec<u8> {
    let n = data.len() as u16;
    let mut out = vec![0x01];
    le16(&mut out, n);
    le16(&mut out, !n);
    out.extend_from_slice(data);
    out
}

/// A complete archive of `members`, with every size and offset in ZIP64 form when `zip64`.
fn zip_bytes(members: &[Member<'_>], zip64: bool) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for m in members {
        let body = if m.deflate {
            stored_deflate(m.data)
        } else {
            m.data.to_vec()
        };
        let method: u16 = if m.deflate { 8 } else { 0 };
        let local_offset = out.len() as u64;
        le32(&mut out, 0x0403_4b50);
        le16(&mut out, 20);
        le16(&mut out, 0);
        le16(&mut out, method);
        le32(&mut out, 0);
        le32(&mut out, 0);
        le32(&mut out, body.len() as u32);
        le32(&mut out, m.data.len() as u32);
        le16(&mut out, m.name.len() as u16);
        le16(&mut out, 0);
        out.extend_from_slice(m.name.as_bytes());
        out.extend_from_slice(&body);
        let host: u16 = if m.mode == 0 { 0 } else { 3 };
        le32(&mut central, 0x0201_4b50);
        le16(&mut central, (host << 8) | 20);
        le16(&mut central, 20);
        le16(&mut central, 0);
        le16(&mut central, method);
        le32(&mut central, 0);
        le32(&mut central, 0);
        let mut extra = Vec::new();
        if zip64 {
            le32(&mut central, 0xFFFF_FFFF);
            le32(&mut central, 0xFFFF_FFFF);
            le16(&mut extra, 0x0001);
            le16(&mut extra, 24);
            le64(&mut extra, m.data.len() as u64);
            le64(&mut extra, body.len() as u64);
            le64(&mut extra, local_offset);
        } else {
            le32(&mut central, body.len() as u32);
            le32(&mut central, m.data.len() as u32);
        }
        le16(&mut central, m.name.len() as u16);
        le16(&mut central, extra.len() as u16);
        le16(&mut central, 0);
        le16(&mut central, 0);
        le16(&mut central, 0);
        le32(&mut central, m.mode << 16);
        le32(
            &mut central,
            if zip64 {
                0xFFFF_FFFF
            } else {
                local_offset as u32
            },
        );
        central.extend_from_slice(m.name.as_bytes());
        central.extend_from_slice(&extra);
    }
    let cd_offset = out.len() as u64;
    let cd_size = central.len() as u64;
    out.extend_from_slice(&central);
    if zip64 {
        let eocd64_offset = out.len() as u64;
        le32(&mut out, 0x0606_4b50);
        le64(&mut out, 44);
        le16(&mut out, 45);
        le16(&mut out, 45);
        le32(&mut out, 0);
        le32(&mut out, 0);
        le64(&mut out, members.len() as u64);
        le64(&mut out, members.len() as u64);
        le64(&mut out, cd_size);
        le64(&mut out, cd_offset);
        le32(&mut out, 0x0706_4b50);
        le32(&mut out, 0);
        le64(&mut out, eocd64_offset);
        le32(&mut out, 1);
        le32(&mut out, 0x0605_4b50);
        le16(&mut out, 0xFFFF);
        le16(&mut out, 0xFFFF);
        le16(&mut out, 0xFFFF);
        le16(&mut out, 0xFFFF);
        le32(&mut out, 0xFFFF_FFFF);
        le32(&mut out, 0xFFFF_FFFF);
        le16(&mut out, 0);
    } else {
        le32(&mut out, 0x0605_4b50);
        le16(&mut out, 0);
        le16(&mut out, 0);
        le16(&mut out, members.len() as u16);
        le16(&mut out, members.len() as u16);
        le32(&mut out, cd_size as u32);
        le32(&mut out, cd_offset as u32);
        le16(&mut out, 0);
    }
    out
}

const MEMBERS: [Member<'static>; 2] = [
    Member {
        name: "xl/workbook.xml",
        mode: 0o100_644,
        data: b"<workbook/>",
        deflate: true,
    },
    Member {
        name: "README",
        mode: 0,
        data: b"read me",
        deflate: false,
    },
];

fn listing(bytes: &[u8]) -> Vec<(Entry, Vec<u8>)> {
    let mut zip = Archive::open(Cursor::new(bytes)).unwrap();
    let entries: Vec<Entry> = zip.entries().collect::<std::io::Result<_>>().unwrap();
    entries
        .into_iter()
        .map(|e| {
            let body = zip.read(&e, 1 << 20).unwrap();
            (e, body)
        })
        .collect()
}

fn refusal(bytes: &[u8]) -> String {
    refusal_reading(bytes, b"README")
}

/// The first error opening `bytes`, walking it, or reading the member `name`.
fn refusal_reading(bytes: &[u8], name: &[u8]) -> String {
    let err = match Archive::open(Cursor::new(bytes)) {
        Err(e) => e,
        Ok(mut zip) => {
            let mut first = None;
            for e in zip.entries() {
                match e {
                    Ok(_) => {}
                    Err(e) => {
                        first = Some(e);
                        break;
                    }
                }
            }
            match first {
                Some(e) => e,
                None => {
                    let e = zip.find(name).unwrap().unwrap();
                    zip.read(&e, 1 << 20).unwrap_err()
                }
            }
        }
    };
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData, "{err}");
    err.to_string()
}

/// Both encodings list the same entries and bodies, in order, with the Unix mode only
/// where a Unix host wrote it. NEGATIVE CONTROL: reading the ZIP64 extra field's
/// sizes in the wrong order (comp before uncomp) cuts the deflated member's body
/// short, so its read fails with "truncated deflate stream".
#[test]
fn stored_deflated_and_zip64_archives_read_alike() {
    let plain = listing(&zip_bytes(&MEMBERS, false));
    let z64 = listing(&zip_bytes(&MEMBERS, true));
    assert_eq!(plain.len(), 2);
    let names: Vec<&[u8]> = plain.iter().map(|(e, _)| e.name.as_slice()).collect();
    assert_eq!(names, [&b"xl/workbook.xml"[..], b"README"]);
    assert_eq!(plain[0].1, b"<workbook/>");
    assert_eq!(plain[1].1, b"read me");
    assert_eq!(plain[0].0.unix_mode(), 0o100_644);
    assert_eq!(plain[1].0.unix_mode(), 0);
    assert!(!plain[0].0.encrypted());
    for ((a, ab), (b, bb)) in plain.iter().zip(&z64) {
        assert_eq!((&a.name, a.uncomp_size, ab), (&b.name, b.uncomp_size, bb));
    }
}

/// `find` returns the first exact name and leaves the walk cursor alone; `body`
/// streams the same bytes `read` returns. NEGATIVE CONTROL: having `find` advance the
/// shared cursor leaves it past every record after the failed `READ` search, so the
/// following `next_entry` yields `None` and its `unwrap` panics.
#[test]
fn find_names_exactly_and_body_streams() {
    let bytes = zip_bytes(&MEMBERS, false);
    let mut zip = Archive::open(Cursor::new(&bytes[..])).unwrap();
    assert_eq!(zip.len(), 2);
    assert!(!zip.is_empty());
    assert!(zip.find(b"READ").unwrap().is_none());
    let readme = zip.find(b"README").unwrap().unwrap();
    let first = zip.next_entry().unwrap().unwrap();
    assert_eq!(first.name, b"xl/workbook.xml");
    let mut streamed = Vec::new();
    zip.body(&first)
        .unwrap()
        .read_to_end(&mut streamed)
        .unwrap();
    assert_eq!(streamed, b"<workbook/>");
    assert_eq!(zip.read(&readme, 7).unwrap(), b"read me");
    assert!(zip.next_entry().unwrap().is_some());
    assert!(zip.next_entry().unwrap().is_none());
}

/// Each malformation is an `InvalidData` refusal with its own `zip:` reason.
/// NEGATIVE CONTROL: dropping `locate`'s trailing-bytes check opens the archive with
/// garbage after its end record and reads README, so `refusal`'s `unwrap_err` panics.
#[test]
fn malformed_archives_are_refused_with_a_reason() {
    let plain = zip_bytes(&MEMBERS, false);
    let cd = plain
        .windows(4)
        .position(|w| w == [0x50, 0x4b, 0x01, 0x02])
        .unwrap();
    let readme_cd = plain.windows(6).rposition(|w| w == b"README").unwrap() - 46;

    assert!(refusal(b"").contains("too short"));
    assert!(refusal(&[0u8; 30]).contains("no end-of-central-directory"));
    let mut trailing = plain.clone();
    trailing.extend_from_slice(b"garbage");
    assert!(refusal(&trailing).contains("trailing bytes"));
    let mut sig = plain.clone();
    sig[cd] = b'Q';
    assert!(refusal(&sig).contains("bad signature"));
    let mut meth = plain.clone();
    meth[readme_cd + 10] = 12;
    assert!(refusal(&meth).contains("unsupported compression method"));
    let mut enc = plain.clone();
    enc[readme_cd + 8] |= 1;
    assert!(refusal(&enc).contains("encrypted"));
    let mut lying = plain.clone();
    lying[cd + 24..cd + 28].copy_from_slice(&3u32.to_le_bytes());
    assert!(
        refusal_reading(&lying, b"xl/workbook.xml").contains("disagrees with its declared size")
    );
    let mut stored = plain.clone();
    stored[readme_cd + 24..readme_cd + 28].copy_from_slice(&3u32.to_le_bytes());
    assert!(refusal(&stored).contains("sizes disagree"));
    let mut overrun = plain.clone();
    overrun[readme_cd + 20..readme_cd + 28].copy_from_slice(&[0, 1, 0, 0, 0, 1, 0, 0]);
    assert!(refusal(&overrun).contains("overruns the central directory"));

    let mut zip = Archive::open(Cursor::new(&plain[..])).unwrap();
    let readme = zip.find(b"README").unwrap().unwrap();
    let err = zip.read(&readme, 6).unwrap_err();
    assert!(err.to_string().contains("cap"), "{err}");
}

/// Records that do not fill the directory's declared size are refused once the last
/// declared record is read. NEGATIVE CONTROL: dropping `next_entry`'s `at != end` check
/// makes the walk end cleanly and fails the `unwrap_err`.
#[test]
fn a_directory_larger_than_its_records_is_refused_at_the_end() {
    let mut bytes = zip_bytes(&MEMBERS[1..], false);
    let eocd = bytes.len() - 22;
    // A pad byte between the directory and the end record, counted in the declared size.
    let cd_size = u32::from_le_bytes(bytes[eocd + 12..eocd + 16].try_into().unwrap());
    bytes[eocd + 12..eocd + 16].copy_from_slice(&(cd_size + 1).to_le_bytes());
    bytes.insert(eocd, 0);
    let mut zip = Archive::open(Cursor::new(&bytes[..])).unwrap();
    assert!(zip.next_entry().unwrap().is_some());
    let err = zip.next_entry().unwrap_err();
    assert!(
        err.to_string().contains("central directory size disagrees"),
        "{err}"
    );
    let err = zip.find(b"nope").unwrap_err();
    assert!(
        err.to_string().contains("central directory size disagrees"),
        "{err}"
    );
}
