// Copyright (c) 2019 Fontations Developers
// Modifications Copyright 2026 Andrew Yates
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from skrifa 0.46.2 src/outline/autohint/shape.rs and read-fonts
// 0.43.2 src/tables/layout/script.rs (`ScriptTags`, `ScriptList::select`,
// `LangSys::feature_index_for_tag`), and modified by the aterm project in
// 2026: walking aterm-font's `GSUB` reader, in skrifa's default "best effort"
// mode only (the nominal mode served upstream's FreeType validation).
// See ../../LICENSE-FONTATIONS for the upstream license.

use super::*;

#[test]
fn simple_script_tag_from_unicode() {
    for unicode_tag in [b"Cyrl", b"Grek", b"Hebr", b"Latn"] {
        let mut bytes = *unicode_tag;
        bytes[0] = bytes[0].to_ascii_lowercase();
        let result = script_tags_from_unicode(Tag::from_bytes(unicode_tag));
        assert_eq!(result, [Tag::from_bytes(&bytes)]);
    }
}

#[test]
fn exception_script_tag_from_unicode() {
    let cases = [
        (b"Kana", b"kana"),
        (b"Hira", b"kana"),
        (b"Nkoo", b"nko "),
        (b"Yiii", b"yi  "),
        (b"Vaii", b"vai "),
    ];
    for (unicode_tag, ot_tag) in cases {
        let result = script_tags_from_unicode(Tag::from_bytes(unicode_tag));
        assert_eq!(result, [Tag::from_bytes(ot_tag)]);
    }
}

#[test]
fn multi_script_tags_from_unicode() {
    let cases: [(&[u8; 4], &[&[u8; 4]]); 4] = [
        (b"Beng", &[b"bng3", b"bng2", b"beng"]),
        (b"Orya", &[b"ory3", b"ory2", b"orya"]),
        (b"Mlym", &[b"mlm3", b"mlm2", b"mlym"]),
        (b"Mymr", &[b"mym2", b"mymr"]),
    ];
    for (unicode_tag, ot_tags) in cases {
        let result = script_tags_from_unicode(Tag::from_bytes(unicode_tag));
        let expected: Vec<Tag> = ot_tags.iter().map(|t| Tag::from_bytes(t)).collect();
        assert_eq!(result, expected);
    }
}

#[test]
fn visited_set() {
    let mut storage = [0u8; 2];
    let mut set = VisitedLookupSet::new(&mut storage);
    assert!(set.insert(0));
    assert!(!set.insert(0));
    assert!(set.insert(15));
    assert!(!set.insert(16));
    set.clear();
    assert!(set.insert(0));
}
