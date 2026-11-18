// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use super::*;

fn canonical(kind: &str) -> Vec<u8> {
    format!("object {}\ntype {kind}\ntag v1\ntagger Fixture <fixture@example.invalid> 1700000000 +0000\n\nmessage\n", "1".repeat(40)).into_bytes()
}

#[test]
fn tag_headers_are_unique_canonical_and_message_bytes_are_opaque() {
    let raw = canonical("commit");
    assert_eq!(tag_target(&raw), Some(("1".repeat(40), Kind::Commit)));
    let mut binary_message = raw.clone();
    binary_message.extend_from_slice(&[0xff, 0xfe]);
    assert_eq!(tag_target(&binary_message), tag_target(&raw));
    assert_eq!(
        tag_target(&canonical("tag")),
        Some(("1".repeat(40), Kind::Tag))
    );
    for (before, after) in [
        ("type commit", "type blob"),
        ("tag v1\n", "tag v1\ntag v2\n"),
        ("type commit\n", "type commit\ntype tag\n"),
        ("object ", "object invalid"),
        ("tag v1", "tag ../v1"),
        ("+0000", "+2560"),
        ("1700000000", "invalid-time"),
        ("1700000000", "18446744073709551615"),
        ("<fixture@example.invalid>", "<bad>address>"),
    ] {
        let altered = String::from_utf8(raw.clone())
            .unwrap()
            .replace(before, after);
        assert!(tag_target(altered.as_bytes()).is_none(), "{altered}");
    }
}

#[test]
fn raw_frames_bind_type_size_and_preserve_every_body_byte_for_native_hashing() {
    let raw = canonical("commit");
    let id = "a".repeat(40);
    let group = [ObjectSize {
        wanted: Wanted {
            id: id.clone(),
            kind: Kind::Tag,
            depth: 0,
        },
        size: raw.len(),
    }];
    let mut stdout = format!("{id} tag {}\n", raw.len()).into_bytes();
    stdout.extend_from_slice(&raw);
    stdout.push(b'\n');
    let good = ChildOut {
        code: Some(0),
        stdout,
        stderr: Vec::new(),
    };
    assert!(bodies(&group, &good).is_some());
    let mut forged = good.clone();
    let offset = forged
        .stdout
        .iter()
        .position(|byte| *byte == b'\n')
        .unwrap()
        + 1;
    forged.stdout[offset + raw.len() - 2] ^= 1;
    assert_ne!(bodies(&group, &forged).unwrap()[0].body, raw);
    for extra in [b"garbage".as_slice(), b"\n"] {
        let mut extra_out = good.clone();
        extra_out.stdout.extend_from_slice(extra);
        assert!(bodies(&group, &extra_out).is_none());
    }
    let mut cut = good.clone();
    cut.stdout.pop();
    assert!(bodies(&group, &cut).is_none());
    let mut wrong_kind = good;
    wrong_kind.stdout.splice(41..44, b"blob".iter().copied());
    assert!(bodies(&group, &wrong_kind).is_none());
    let saturated = ChildOut {
        code: Some(0),
        stdout: vec![b'x'; ChildOut::OUTPUT_KEEP],
        stderr: Vec::new(),
    };
    assert!(bodies(&group, &saturated).is_none());
    assert!(inventory(&[group[0].wanted.clone()], &saturated).is_none());
}
