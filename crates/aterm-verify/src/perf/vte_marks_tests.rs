// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// How many of `text`'s code points join the glyph before them, in the ways the corpora
/// exercise: one aterm's width table gives no cell (a non-spacing mark, a conjoining vowel
/// or trailing jamo, ZWJ, VS16), a skin tone, a flag's second regional indicator, or the
/// emoji after a ZWJ. Devanagari's spacing marks (U+0903, U+093B, U+093E-U+0940,
/// U+0949-U+094C, U+094E, U+094F) are marks too, but that table gives each a cell of its
/// own, so they are not counted.
fn joining(text: &str) -> usize {
    let (mut count, mut after_zwj, mut open_flag) = (0, false, false);
    for c in text.chars() {
        let cp = u32::from(c);
        let indicator = (0x1F1E6..=0x1F1FF).contains(&cp);
        let joins = after_zwj
            || (indicator && open_flag)
            || matches!(
                cp,
                0x300..=0x36F
                    | 0x900..=0x902
                    | 0x93A
                    | 0x93C
                    | 0x941..=0x948
                    | 0x94D
                    | 0xE31
                    | 0xE34..=0xE3A
                    | 0xE47..=0xE4E
                    | 0x1160..=0x11FF
                    | 0x200D
                    | 0xFE0F
                    | 0x1F3FB..=0x1F3FF
            );
        count += usize::from(joins);
        open_flag = indicator && !open_flag;
        after_zwj = cp == 0x200D;
    }
    count
}

#[test]
fn only_what_takes_no_cell_of_its_own_is_counted_as_joining() {
    // कि: a spacing vowel sign; कु: a non-spacing one; क्: a virama.
    assert_eq!(joining("\u{915}\u{93F} \u{915}\u{941} \u{915}\u{94D}"), 2);
    // Two flags, four regional indicators: each flag's second joins its first.
    assert_eq!(joining("\u{1F1FA}\u{1F1F8}\u{1F1EF}\u{1F1F5}"), 2);
    // A ZWJ sequence: the joiner and the emoji after it.
    assert_eq!(joining("\u{1F468}\u{200D}\u{1F4BB}"), 2);
}

#[test]
fn every_corpus_is_mark_heavy() {
    let corpora = corpora();
    let keys: Vec<String> = corpora
        .iter()
        .map(|(name, _)| format!("{name}_best_mib_per_gcycle"))
        .collect();
    assert_eq!(keys, LANE.keys, "one gated key per corpus, in order");
    for (name, unit) in &corpora {
        let joining = joining(unit);
        let printing = unit.chars().filter(|c| !c.is_ascii_whitespace()).count();
        assert!(
            joining * 6 >= printing,
            "{name}: {joining} joining of {printing} printing code points"
        );
    }
}

#[test]
fn a_syllable_is_spelled_in_its_conjoining_jamo() {
    assert_eq!(conjoining("한"), "\u{1112}\u{1161}\u{11AB}");
    assert_eq!(conjoining("가"), "\u{1100}\u{1161}");
    assert_eq!(conjoining("힣"), "\u{1112}\u{1175}\u{11C2}");
    assert_eq!(conjoining("a 가\r\n"), "a \u{1100}\u{1161}\r\n");
}

#[test]
fn the_corpora_are_written_whole_and_named_by_their_bytes() {
    let corpora = corpora();
    let dir = Scratch::new("atv-vte-marks").expect("scratch");
    write_corpora(dir.path(), &corpora).expect("writes");
    for (name, unit) in &corpora {
        let bytes = std::fs::read(dir.path().join(format!("{name}.vt"))).expect("reads");
        let target = CORPUS_MB << 20;
        assert!(
            bytes.len() as u64 >= target && bytes.len().is_multiple_of(unit.len()),
            "{name}"
        );
        assert!(bytes.starts_with(unit.as_bytes()), "{name}");
    }
    let mut other = corpora.clone();
    other[0].1.push('x');
    assert_ne!(digest(&corpora), digest(&other));
}
