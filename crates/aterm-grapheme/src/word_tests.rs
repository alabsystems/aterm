// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

/// The code points a UCD `Derived*.txt` file gives one of `values`.
fn ucd(data: &str, values: &[&str]) -> Vec<bool> {
    let mut set = vec![false; 0x11_0000];
    for line in data.lines() {
        let content = line.split('#').next().unwrap_or("");
        let Some((range, value)) = content.split_once(';') else {
            continue;
        };
        if !values.contains(&value.trim()) {
            continue;
        }
        let range = range.trim();
        let (lo, hi) = range.split_once("..").unwrap_or((range, range));
        let lo = u32::from_str_radix(lo, 16).expect("a hex code point");
        let hi = u32::from_str_radix(hi, 16).expect("a hex code point");
        for cp in lo..=hi {
            set[cp as usize] = true;
        }
    }
    assert!(set.iter().any(|&b| b), "no {values:?} code points parsed");
    set
}

/// `ours` answers exactly `ucd` for every scalar value.
fn agrees(name: &str, ours: fn(char) -> bool, ucd: &[bool]) {
    let (missing, extra): (Vec<char>, Vec<char>) = (0..=0x10_FFFF)
        .filter_map(char::from_u32)
        .filter(|&c| ours(c) != ucd[c as usize])
        .partition(|&c| ucd[c as usize]);
    assert!(
        missing.is_empty() && extra.is_empty(),
        "{name}: {} missing, e.g. {:?}; {} extra, e.g. {:?}",
        missing.len(),
        &missing[..missing.len().min(8)],
        extra.len(),
        &extra[..extra.len().min(8)],
    );
}

/// The virama table is the UCD's `Canonical_Combining_Class=9`, every code
/// point, in the Unicode version of the crate's other tables.
#[test]
fn the_virama_table_is_the_ucds_combining_class_9() {
    let data = include_str!("../tests/data/DerivedCombiningClass-16.0.0.txt");
    agrees("virama", is_virama, &ucd(data, &["9"]));
}

/// The cursive-joining table is the UCD's `Joining_Type` D, L, R and C,
/// every code point.
#[test]
fn the_joining_table_is_the_ucds_joining_letters() {
    let data = include_str!("../tests/data/DerivedJoiningType-16.0.0.txt");
    agrees(
        "joins cursively",
        joins_cursively,
        &ucd(data, &["D", "L", "R", "C"]),
    );
}

/// The words of `text` under the default classifier.
fn split(text: &str) -> Vec<&str> {
    words(text, is_word_char).map(|word| &text[word]).collect()
}

/// The words of `text` when only a space separates words.
fn split_at_spaces(text: &str) -> Vec<&str> {
    words(text, |c| c != ' ').map(|word| &text[word]).collect()
}

/// A joiner belongs to a word exactly where it spells it: next to a virama,
/// between two letters of a cursive-joining script, inside an emoji ZWJ
/// sequence.
#[test]
fn a_joiner_belongs_to_a_word_only_where_it_spells_it() {
    for word in [
        "\u{0645}\u{06CC}\u{200C}\u{062E}\u{0648}\u{0627}\u{0647}\u{0645}", // می‌خواهم
        "\u{062F}\u{06CC}\u{0648}\u{200C}\u{0647}\u{0627}", // دیو‌ها: after a right-joining letter
        "\u{0628}\u{064E}\u{200C}\u{0647}", // a harakat between the letter and the ZWNJ
        "\u{0926}\u{0930}\u{094D}\u{200D}\u{092F}\u{093E}", // दर्‍या: eyelash ra
        "\u{0DC1}\u{0DCA}\u{200D}\u{0DBB}\u{0DD3}", // ශ්‍රී
        "\u{0915}\u{094D}\u{200D}\u{0937}", // क्‍ष: half form
        "\u{0915}\u{094D}\u{200C}\u{0937}", // क्‌ष: explicit virama
        "\u{09B0}\u{200D}\u{09CD}\u{09AF}\u{09BE}\u{09AC}", // র‍্যাব: ya-phala
        "\u{0CB0}\u{200D}\u{0CCD}\u{0C95}", // ರ‍್ಕ: Kannada non-repha ra
        "\u{182C}\u{1820}\u{1837}\u{180E}\u{1820}", // ᠬᠠᠷ᠎ᠠ: Mongolian vowel separator
        "\u{1828}\u{1822}\u{180B}\u{1826}\u{180E}\u{1821}", // a free variation selector inside
        "\u{13000}\u{13430}\u{13001}",      // an Egyptian vertical joiner
        "\u{13000}\u{13431}\u{13437}\u{13001}\u{13430}\u{13002}\u{13438}\u{13003}", // a nested quadrat
        "\u{1BC00}\u{1BCA0}\u{1BC01}\u{1BCA2}\u{1BC02}", // Duployan overlap and step
    ] {
        assert_eq!(split(&format!("x {word} y")), ["x", word, "y"], "{word:?}");
        assert_eq!(
            split_at_spaces(&format!("x {word} y")),
            ["x", word, "y"],
            "{word:?}"
        );
    }
    for (text, parts) in [
        ("foo\u{200D}bar", &["foo", "bar"][..]),
        ("foo\u{200C}bar", &["foo", "bar"]),
        ("e\u{301}\u{200D}f", &["e\u{301}", "f"]),
        ("\u{0628}\u{200D}x", &["\u{0628}", "x"]),
        ("x\u{200C}\u{0628}", &["x", "\u{0628}"]),
        ("\u{0915}\u{094D}\u{200D} x", &["\u{0915}\u{094D}", "x"]),
        ("x \u{200C}\u{0915}", &["x", "\u{0915}"]),
        ("\u{200D}\u{0915}", &["\u{0915}"]),
        (
            "\u{0915}\u{094D}\u{200D}\u{200C}\u{0937}",
            &["\u{0915}\u{094D}", "\u{0937}"],
        ),
        (
            "\u{0628}\u{200C}\u{200C}\u{0628}",
            &["\u{0628}", "\u{0628}"],
        ),
        ("ab\u{180E}cd", &["ab", "cd"]),
        ("\u{0628}\u{180E}\u{0628}", &["\u{0628}", "\u{0628}"]),
        ("\u{1820}\u{180E}x", &["\u{1820}", "x"]),
        (
            "\u{1820}\u{180E} \u{180E}\u{1820}",
            &["\u{1820}", "\u{1820}"],
        ),
        ("ab\u{13430}cd", &["ab", "cd"]),
        ("\u{1343C}\u{13000}\u{1343D} x", &["\u{13000}", "x"]),
        ("\u{13000}\u{180E}\u{13001}", &["\u{13000}", "\u{13001}"]),
        ("ab\u{1BCA0}cd", &["ab", "cd"]),
    ] {
        assert_eq!(split(text), parts, "{text:?}");
    }
    let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
    assert_eq!(
        split_at_spaces(&format!("a{family}b c")),
        [&format!("a{family}b"), "c"]
    );
    assert_eq!(split_at_spaces("a\u{200D}\u{1F469}"), ["a", "\u{1F469}"]);
    assert_eq!(split(family), Vec::<&str>::new(), "emoji are no letters");
}

/// Marks belong to the character before them and prepended marks to the one
/// after; every other format control separates words, under any classifier.
#[test]
fn marks_attach_and_format_controls_separate() {
    assert_eq!(split("cafe\u{301} x"), ["cafe\u{301}", "x"]);
    assert_eq!(
        split("x \u{301}\u{301}y"),
        ["x", "y"],
        "a mark after a space"
    );
    assert_eq!(split("\u{301}"), Vec::<&str>::new());
    assert_eq!(
        split("\u{1B13}\u{1B44}\u{1B31}"),
        ["\u{1B13}\u{1B44}\u{1B31}"],
        "a spacing virama"
    );
    assert_eq!(
        split("x \u{06DD}\u{0661}\u{0662}"),
        ["x", "\u{06DD}\u{0661}\u{0662}"]
    );
    assert_eq!(
        split("\u{0600}\u{0600}1"),
        ["\u{0600}\u{0600}1"],
        "a run of prepended marks"
    );
    assert_eq!(
        split("a\u{06DD} \u{06DD}"),
        ["a"],
        "a prepended mark before nothing"
    );
    for control in [
        '\u{061C}',
        '\u{200B}',
        '\u{200E}',
        '\u{200F}',
        '\u{202A}',
        '\u{202E}',
        '\u{2060}',
        '\u{2064}',
        '\u{2066}',
        '\u{2069}',
        '\u{FEFF}',
        '\u{FFF9}',
        '\u{FFFA}',
        '\u{FFFB}',
        '\u{1D173}',
        '\u{1D17A}',
        '\u{E0001}',
        '\t',
        '\n',
        '\u{85}',
    ] {
        let text = format!("ab{control}cd");
        assert_eq!(split(&text), ["ab", "cd"], "U+{:04X}", control as u32);
        assert_eq!(
            split_at_spaces(&text),
            ["ab", "cd"],
            "U+{:04X}",
            control as u32
        );
    }
    assert_eq!(split("foo\nbar_1"), ["foo", "bar_1"]);
    assert_eq!(split_at_spaces("a-b c"), ["a-b", "c"]);
}

/// The walk is one pass: however marks, joiners and prepended marks pile up,
/// it classifies each character a bounded number of times.
#[test]
fn the_walk_classifies_each_character_a_bounded_number_of_times() {
    let marks = "\u{064E}".repeat(512);
    for text in [
        format!("\u{0628}\u{200C}{marks}\u{0628}").repeat(256),
        "\u{0628}\u{200C}".repeat(64 * 1024),
        format!("{}1", "\u{0600}".repeat(128 * 1024)),
        format!("\u{0915}{}", "\u{094D}\u{200D}".repeat(64 * 1024)),
        format!("\u{13000}{}\u{13001}", "\u{13430}".repeat(128 * 1024)),
        "\u{1820}\u{180E}".repeat(64 * 1024),
    ] {
        ROLE_LOOKUPS.with(|n| n.set(0));
        let count = words(&text, is_word_char).count();
        let lookups = ROLE_LOOKUPS.with(std::cell::Cell::get);
        let chars = text.chars().count();
        assert!(count >= 1);
        assert!(
            lookups <= 3 * chars,
            "{lookups} classifications for {chars} characters"
        );
    }
    let text = "ab.".repeat(64 * 1024);
    ROLE_LOOKUPS.with(|n| n.set(0));
    assert_eq!(
        word_break_before(&text, text.len() - 1, is_word_char),
        text.len() - 4
    );
    assert!(ROLE_LOOKUPS.with(std::cell::Cell::get) <= 3);
}

/// A caret's word-left motion lands where the whole walk says the last word
/// before it starts, reading back no further than the break before that
/// word.
#[test]
fn the_word_before_a_caret_is_the_last_word_that_starts_before_it() {
    let classifiers: [fn(char) -> bool; 2] = [is_word_char, |c| c != ' '];
    for text in [
        "x \u{06DD}\u{0661}  .\u{301}a \u{0600}.\u{0600}1",
        "foo\u{200D}bar \u{0628}\u{200C}\u{0628}-\u{0628}\u{200C}x",
        "\u{0915}\u{094D}\u{200D}\u{0937}-\u{0915}\u{094D}\u{200D} y\u{2066}z",
        "\u{13000}\u{13431}\u{13437}\u{13001}\u{13438} \u{1820}\u{180E}\u{1820}",
        "  ",
        "",
    ] {
        for is_word in classifiers {
            let all: Vec<Range<usize>> = words(text, is_word).collect();
            for position in 0..=text.len() {
                let floor = (0..=position)
                    .rev()
                    .find(|&at| text.is_char_boundary(at))
                    .unwrap_or(0);
                let expected = all
                    .iter()
                    .rev()
                    .map(|word| word.start)
                    .find(|&start| start < floor);
                assert_eq!(
                    word_start_before(text, position, is_word),
                    expected,
                    "{text:?} at {position}"
                );
            }
        }
    }
    let text = "ab.".repeat(64 * 1024);
    ROLE_LOOKUPS.with(|n| n.set(0));
    assert_eq!(
        word_start_before(&text, text.len(), is_word_char),
        Some(text.len() - 3)
    );
    assert!(ROLE_LOOKUPS.with(std::cell::Cell::get) <= 8);
    let text = format!("ab{}", " ".repeat(64 * 1024));
    ROLE_LOOKUPS.with(|n| n.set(0));
    assert_eq!(word_start_before(&text, text.len(), is_word_char), Some(0));
    let lookups = ROLE_LOOKUPS.with(std::cell::Cell::get);
    assert!(lookups <= 3 * text.len(), "{lookups} classifications");
}

/// From the last word break before any position, the walk finds exactly the
/// words of the whole text that start there or later, every word that ends
/// past the position among them, under either classifier: a caret's words
/// can be read from the break instead of from the start of the line.
#[test]
fn the_words_after_a_break_are_the_words_of_the_whole_text() {
    let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
    let corpus = [
        "x \u{06DD}\u{0661} .\u{301}a \u{0600}.\u{0600}1".to_string(),
        "foo\u{200D}bar \u{0628}\u{200C}\u{0628}-\u{0628}\u{200C}x".to_string(),
        "\u{0645}\u{06CC}\u{200C}\u{062E}\u{0648}\u{0627}\u{0647}\u{0645}.ab\u{200B}cd".to_string(),
        "\u{0915}\u{094D}\u{200D}\u{0937}-\u{0915}\u{094D}\u{200D} y\u{2066}z".to_string(),
        "\u{13000}\u{13431}\u{13437}\u{13001}\u{13438} \u{1820}\u{180E}\u{1820}\u{180E}"
            .to_string(),
        format!("a{family}b {family}.c\td\ne"),
        String::new(),
    ];
    let classifiers: [fn(char) -> bool; 2] = [is_word_char, |c| c != ' '];
    for text in &corpus {
        for is_word in classifiers {
            let all: Vec<Range<usize>> = words(text, is_word).collect();
            for position in 0..=text.len() {
                let at = word_break_before(text, position, is_word);
                assert!(at <= position, "{text:?} at {position}");
                let from_break: Vec<Range<usize>> = words(&text[at..], is_word)
                    .map(|word| word.start + at..word.end + at)
                    .collect();
                let after: Vec<Range<usize>> = all
                    .iter()
                    .filter(|word| word.start >= at)
                    .cloned()
                    .collect();
                assert_eq!(from_break, after, "{text:?} from {at} ({position})");
                assert!(
                    all.iter()
                        .filter(|word| word.end > position)
                        .all(|word| word.start >= at),
                    "{text:?}: a word before {at} ends past {position}"
                );
            }
        }
    }
}
