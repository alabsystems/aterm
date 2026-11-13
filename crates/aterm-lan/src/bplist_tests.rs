// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::write::list;
use super::*;

#[test]
fn objects_read_back_as_written() {
    let objects = vec![
        Object::Dict(vec![(1, 2), (3, 4)]),
        Object::String("$objects".into()),
        Object::Array(vec![5, 6, 7, 8]),
        Object::String("long key past fifteen bytes".into()),
        Object::Uid(9),
        Object::Null,
        Object::Bool(true),
        Object::Int(-3),
        Object::String("naïve".into()),
        Object::Bool(false),
    ];
    let bytes = list(&objects, 0);
    let plist = Plist::parse(&bytes).unwrap();
    assert_eq!(plist.top(), 0);
    for (i, object) in objects.iter().enumerate() {
        assert_eq!(&plist.object(i).unwrap(), object, "object {i}");
    }
    assert_eq!(plist.value(0, "$objects"), Some(2));
    assert_eq!(plist.value(0, "long key past fifteen bytes"), Some(4));
    assert_eq!(plist.value(0, "absent"), None);
    assert_eq!(plist.value(2, "$objects"), None, "an array has no keys");
}

/// Truncations, a bad header and references past the table all read as errors, never a
/// panic: every prefix of a list, and a list whose reference points past its table.
#[test]
fn malformed_lists_are_errors_not_panics() {
    let bytes = list(
        &[
            Object::Dict(vec![(1, 2)]),
            Object::String("k".into()),
            Object::Array(vec![1]),
        ],
        0,
    );
    assert!(Plist::parse(&bytes).is_ok());
    for cut in 0..bytes.len() {
        if let Ok(plist) = Plist::parse(&bytes[..cut]) {
            for i in 0..3 {
                let _ = plist.object(i);
            }
        }
    }
    assert!(Plist::parse(b"bplist01").is_err());
    let mut wrong = bytes.clone();
    wrong[..8].copy_from_slice(b"xplist00");
    assert!(Plist::parse(&wrong).is_err());
    // The array's one reference (2 bytes after its marker) aimed past the table.
    let array_at = Plist::parse(&bytes).unwrap().offsets[2];
    let mut past = bytes;
    assert_eq!(past[array_at], 0xA1);
    past[array_at + 1..array_at + 3].copy_from_slice(&40u16.to_be_bytes());
    let plist = Plist::parse(&past).unwrap();
    assert!(plist.object(2).is_err());
}

/// nehelper's own files.
const NEHELPER: [&[u8]; 3] = [
    include_bytes!("fixtures/nehelper-unanswered.plist"),
    include_bytes!("fixtures/nehelper-dismissed.plist"),
    include_bytes!("fixtures/nehelper-granted.plist"),
];

/// A list of what this reader cannot model, by hand, with one-byte offsets and references:
/// a set of two (0), a real (1), a date (2), three bytes of data (3), two UTF-16 units (4),
/// a 16-byte integer (5) and an ASCII string whose count is an integer object (6).
fn unmodelled() -> Vec<u8> {
    let mut bytes = b"bplist00".to_vec();
    let mut offsets = Vec::new();
    let objects: [&[u8]; 7] = [
        &[0xC2, 1, 2],
        &[0x23, 0x40, 0x09, 0x21, 0xFB, 0x54, 0x44, 0x2D, 0x18],
        &[0x33, 0x41, 0xC0, 0, 0, 0, 0, 0, 0],
        &[0x43, 1, 2, 3],
        &[0x62, 0x00, 0x61, 0x00, 0xEF],
        &[0x14, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 42],
        &[0x5F, 0x10, 0x14],
    ];
    for object in objects {
        offsets.push(u8::try_from(bytes.len()).unwrap());
        bytes.extend_from_slice(object);
        if object[0] == 0x5F {
            bytes.extend_from_slice(b"twenty ascii bytes!!");
        }
    }
    let table = bytes.len() as u64;
    bytes.extend(offsets);
    bytes.extend([0, 0, 0, 0, 0, 0, 1, 1]);
    for n in [7u64, 0, table] {
        bytes.extend(n.to_be_bytes());
    }
    bytes
}

/// A rewrite that changes nothing gives back the very bytes nehelper wrote (its data objects
/// and 16-byte integer, which this reader cannot model, among them), and those of a list
/// holding every kind this reader does not model: every object's span is read whole, and
/// the table and trailer laid out as CF lays them out.
#[test]
fn a_rewrite_of_nothing_is_the_list_itself() {
    let unmodelled = unmodelled();
    for bytes in NEHELPER.into_iter().chain([unmodelled.as_slice()]) {
        let plist = Plist::parse(bytes).unwrap();
        let same = plist
            .rewrite(&std::collections::BTreeMap::new(), &[])
            .unwrap();
        assert!(
            same == bytes,
            "{} bytes in, {} out",
            bytes.len(),
            same.len()
        );
    }
}

/// A rewrite encodes what it replaces and appends, keeps every other object as it was, and
/// refuses what it cannot write. NEGATIVE CONTROLS: an index past the table, an object it
/// cannot encode, and more objects than one-byte references reach are errors.
#[test]
fn a_rewrite_changes_only_what_it_names() {
    let bytes = NEHELPER[0];
    let plist = Plist::parse(bytes).unwrap();
    let old = plist.len();
    let top = plist.top();
    let Object::Dict(mut pairs) = plist.object(top).unwrap() else {
        panic!("the top object is a dictionary");
    };
    pairs.push((old, old + 1));
    let replaced = std::collections::BTreeMap::from([(top, Object::Dict(pairs))]);
    let appended = [Object::String("added".into()), Object::Int(1 << 40)];
    let out = plist.rewrite(&replaced, &appended).unwrap();
    let after = Plist::parse(&out).unwrap();
    assert_eq!(after.len(), old + 2);
    assert_eq!(after.value(after.top(), "added"), Some(old + 1));
    assert_eq!(after.object(old + 1).unwrap(), Object::Int(1 << 40));
    for i in (0..old).filter(|i| *i != top) {
        assert_eq!(after.span(i).unwrap(), plist.span(i).unwrap(), "object {i}");
    }
    let past = std::collections::BTreeMap::from([(old, Object::Null)]);
    assert!(plist.rewrite(&past, &[]).is_err());
    assert!(
        plist
            .rewrite(&std::collections::BTreeMap::new(), &[Object::Other])
            .is_err()
    );
    // A list whose references are one byte: an array of one reference, then a null.
    let mut narrow = b"bplist00".to_vec();
    narrow.extend([0xA1, 0x01, 0x00, 8, 10]);
    narrow.extend([0, 0, 0, 0, 0, 0, 1, 1]);
    for n in [2u64, 0, 11] {
        narrow.extend(n.to_be_bytes());
    }
    let one_byte = Plist::parse(&narrow).unwrap();
    let many = vec![Object::Null; 300];
    assert!(
        one_byte
            .rewrite(&std::collections::BTreeMap::new(), &many)
            .is_err()
    );
    assert!(
        one_byte
            .rewrite(&std::collections::BTreeMap::new(), &many[..2])
            .is_ok()
    );
}

/// CF reads a list only when its widths reach its object count and its table, so a rewrite
/// widens its offsets as its table moves past them, as CF's writer does, and refuses an
/// object count its references do not reach; the reader refuses what CF refuses.
/// NEGATIVE CONTROLS: the count one past what one-byte references reach, and a table past
/// one-byte offsets, are refused by the writer and the reader alike.
#[test]
fn widths_reach_the_count_and_the_table_as_cf_requires() {
    let none = std::collections::BTreeMap::new();
    let narrow = |count: usize, offset: usize| {
        let mut bytes = b"bplist00".to_vec();
        bytes.extend(std::iter::repeat_n(0x00, count));
        let table = bytes.len();
        for i in 0..count {
            push_be(&mut bytes, (8 + i) as u64, offset);
        }
        bytes.extend([0, 0, 0, 0, 0, 0]);
        bytes.extend([u8::try_from(offset).unwrap(), 1]);
        for n in [count, 0, table] {
            bytes.extend((n as u64).to_be_bytes());
        }
        bytes
    };
    // One-byte references reach 255 objects and not 256.
    assert!(Plist::parse(&narrow(255, 2)).is_ok());
    assert!(Plist::parse(&narrow(256, 2)).is_err());
    let two_bytes = narrow(2, 1);
    let two = Plist::parse(&two_bytes).unwrap();
    assert!(two.rewrite(&none, &vec![Object::Null; 253]).is_ok());
    assert!(two.rewrite(&none, &vec![Object::Null; 254]).is_err());
    // A string that carries the table past 255 widens the offsets to two bytes.
    let long = [Object::String("x".repeat(250))];
    let wide = two.rewrite(&none, &long).unwrap();
    let trailer = wide.len() - 32;
    assert_eq!(
        wide[trailer + 6],
        2,
        "offsets widened past a table at 256 and on"
    );
    assert_eq!(Plist::parse(&wide).unwrap().object(2).unwrap(), long[0]);
    // The same list with one-byte offsets, as the last object's start alone would size them.
    let mut cut = wide[..trailer - 3 * 2].to_vec();
    cut.extend([8, 9, 10]);
    cut.extend(&wide[trailer..]);
    let trailer = cut.len() - 32;
    cut[trailer + 6] = 1;
    assert!(
        Plist::parse(&cut).is_err(),
        "a table at 263 past one-byte offsets"
    );
}
