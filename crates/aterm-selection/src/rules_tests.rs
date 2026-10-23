// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

#[test]
fn test_file_path_bare_relative_is_whole_not_fabricated_absolute() {
    let rule = BuiltinRules::file_path();
    for (text, want) in [
        (
            "see crates/aterm-gui/src/lib.rs for it",
            "crates/aterm-gui/src/lib.rs",
        ),
        ("open src/main.rs now", "src/main.rs"),
        ("cd ~/aterm/target/", "~/aterm/target/"),
        ("edit ../sibling/file.txt", "../sibling/file.txt"),
        ("cat /etc/hosts", "/etc/hosts"),
    ] {
        let matches: Vec<_> = rule.find_all(text).collect();
        assert_eq!(matches.len(), 1, "{text}: {matches:?}");
        assert_eq!(matches[0].as_str(), want, "{text}");
        assert!(!matches[0].as_str().starts_with("/aterm-gui"), "{text}");
    }
    assert_eq!(rule.find_all("just main.rs here").count(), 0);
}

#[test]
fn uuid_outranks_git_hash_on_first_and_last_hex_groups() {
    let smart = SmartSelection::with_builtin_rules();
    let uuid = "550e8400-e29b-41d4-a716-446655440000";

    let m = smart
        .find_at(uuid, 2)
        .expect("find_at inside first UUID group");
    assert_eq!(m.matched_text(), uuid, "first group selects whole UUID");
    assert_eq!((m.start(), m.end()), (0, uuid.len()));
    assert_eq!(m.kind(), SelectionRuleKind::Uuid);

    let m = smart
        .find_at(uuid, 30)
        .expect("find_at inside last UUID group");
    assert_eq!(m.matched_text(), uuid, "last group selects whole UUID");
    assert_eq!((m.start(), m.end()), (0, uuid.len()));
    assert_eq!(m.kind(), SelectionRuleKind::Uuid);
}

#[test]
fn standalone_git_hash_still_matches_with_uuid_at_high() {
    let smart = SmartSelection::with_builtin_rules();
    let hash = "abcdef0123456789abcdef0123456789abcdef01";
    let m = smart.find_at(hash, 5).expect("find_at inside git hash");
    assert_eq!(m.matched_text(), hash);
    assert_eq!(m.kind(), SelectionRuleKind::GitHash);
}

#[test]
fn builtin_patterns_match_their_tokens() {
    for (rule, text, want) in [
        (
            BuiltinRules::ipv4(),
            "Server at 192.168.1.100:8080 is up",
            "192.168.1.100:8080",
        ),
        (
            BuiltinRules::git_hash(),
            "Commit abc1234 fixed the bug",
            "abc1234",
        ),
        (
            BuiltinRules::double_quoted_string(),
            r#"echo "hello \"world\"" done"#,
            r#""hello \"world\"""#,
        ),
        (BuiltinRules::semver(), "version 1.2.3 released", "1.2.3"),
        (
            BuiltinRules::semver(),
            "version 1.0.0-alpha released",
            "1.0.0-alpha",
        ),
        (
            BuiltinRules::semver(),
            "Version v1.2.3-beta.1+build.456 released",
            "v1.2.3-beta.1+build.456",
        ),
        (
            BuiltinRules::email(),
            "Contact john.doe+test@sub.example.co.uk for info",
            "john.doe+test@sub.example.co.uk",
        ),
    ] {
        let got: Vec<&str> = rule.find_all(text).map(|m| m.as_str()).collect();
        assert_eq!(got, [want], "{text:?}");
    }
}

#[test]
fn builtin_patterns_reject_lookalikes() {
    for (rule, texts) in [
        (
            BuiltinRules::url(),
            &[
                "just some plain text",
                "not-a-url",
                "foo.bar.baz",
                "//no-scheme.example.com",
                "mailto:user@example.com",
            ][..],
        ),
        (
            BuiltinRules::email(),
            &[
                "not an email",
                "@missing-local.com",
                "user@",
                "user@.com",
                "user@com",
                "user@@double.com",
            ][..],
        ),
        (
            BuiltinRules::ipv4(),
            &["999.999.999.999", "1.2.3", "not an ip address"][..],
        ),
        (
            BuiltinRules::git_hash(),
            &["abc123", "not hex at all", "12345"][..],
        ),
        (
            BuiltinRules::uuid(),
            &[
                "not-a-uuid-at-all",
                "550e8400-e29b-41d4-a716",
                "550e8400e29b41d4a716446655440000",
                "ZZZZZZZZ-ZZZZ-ZZZZ-ZZZZ-ZZZZZZZZZZZZ",
            ][..],
        ),
        (
            BuiltinRules::file_path(),
            &["just a sentence", "no-slashes-here", "123.456"][..],
        ),
    ] {
        for text in texts {
            let got: Vec<&str> = rule.find_all(text).map(|m| m.as_str()).collect();
            assert!(got.is_empty(), "{} matched {text:?}: {got:?}", rule.name);
        }
    }
}

#[test]
fn basic_word_boundaries_over_scripts() {
    let hebrew = "\u{05E9}\u{05B8}\u{05C1}\u{05DC}\u{05D5}\u{05B9}\u{05DD}";
    let qamats = hebrew.find('\u{05B8}').expect("qamats");
    for (text, pos, want) in [
        ("hello world_test foo", 2, Some((0, 5))),
        ("hello world_test foo", 8, Some((6, 16))),
        ("hello world_test foo", 5, None),
        ("\u{65E5}\u{672C}\u{8A9E}", 3, Some((0, 9))),
        ("caf\u{E9}", 3, Some((0, 5))),
        ("cafe\u{301}", 5, Some((0, 6))),
        ("\u{0301}", 0, None),
        (hebrew, qamats, Some((0, hebrew.len()))),
        (
            "\u{41F}\u{440}\u{438}\u{432}\u{435}\u{442}",
            0,
            Some((0, 12)),
        ),
        ("hello \u{65E5}\u{672C}\u{8A9E} world", 6, Some((6, 15))),
        ("hello \u{65E5}\u{672C}\u{8A9E} world", 16, Some((16, 21))),
        ("\u{E9}", 1, Some((0, 2))),
    ] {
        assert_eq!(
            SmartSelection::basic_word_boundaries(text, pos),
            want,
            "{text:?} @ {pos}"
        );
    }
}

const JOINED_WORDS: [(&str, &str); 12] = [
    (
        "Persian ZWNJ",
        "\u{0645}\u{06CC}\u{200C}\u{062E}\u{0648}\u{0627}\u{0647}\u{0645}",
    ),
    (
        "Persian ZWNJ after a right-joining letter",
        "\u{062F}\u{06CC}\u{0648}\u{200C}\u{0647}\u{0627}",
    ),
    (
        "Marathi eyelash ra",
        "\u{0926}\u{0930}\u{094D}\u{200D}\u{092F}\u{093E}",
    ),
    ("Sinhala sri", "\u{0DC1}\u{0DCA}\u{200D}\u{0DBB}\u{0DD3}"),
    (
        "Devanagari half form",
        "\u{0915}\u{094D}\u{200D}\u{0937}\u{093E}",
    ),
    (
        "Devanagari explicit virama",
        "\u{0915}\u{094D}\u{200C}\u{0937}",
    ),
    (
        "Bengali ya-phala",
        "\u{09B0}\u{200D}\u{09CD}\u{09AF}\u{09BE}\u{09AC}",
    ),
    (
        "Hindi virama",
        "\u{0928}\u{092E}\u{0938}\u{094D}\u{0924}\u{0947}",
    ),
    ("prepended end of ayah", "\u{06DD}\u{0661}\u{0662}\u{0663}"),
    (
        "Mongolian vowel separator",
        "\u{182C}\u{1820}\u{1837}\u{180E}\u{1820}",
    ),
    (
        "Egyptian hieroglyph quadrat",
        "\u{13000}\u{13431}\u{13437}\u{13001}\u{13430}\u{13002}\u{13438}\u{13003}",
    ),
    ("Duployan letter overlap", "\u{1BC00}\u{1BCA0}\u{1BC01}"),
];

#[test]
fn a_joiner_that_spells_a_word_belongs_to_it() {
    let mut separated = SmartSelection::new();
    separated.set_word_separators(Some(" "));
    for (name, word) in JOINED_WORDS {
        let text = format!("x {word} y");
        let span = Some((2, 2 + word.len()));
        for (i, _) in word.char_indices() {
            assert_eq!(
                SmartSelection::basic_word_boundaries(&text, 2 + i),
                span,
                "{name}: byte {i}"
            );
            assert_eq!(
                separated.word_boundaries_at(&text, 2 + i),
                span,
                "{name}: byte {i}, separators"
            );
        }
        let columns = aterm_grapheme::str_grid_columns(&text);
        let cells = Some((2, columns - 2));
        for smart in [&SmartSelection::with_builtin_rules(), &separated] {
            for column in 2..columns - 2 {
                assert_eq!(
                    smart.word_boundaries_at_column(&text, column),
                    cells,
                    "{name}: column {column}"
                );
            }
        }
    }
}

#[test]
fn an_invisible_character_between_two_words_still_splits_them() {
    for (left, invisible, right) in [
        ("foo", '\u{200D}', "bar"),
        ("foo", '\u{200C}', "bar"),
        ("abc", '\u{202E}', "def"),
        ("a", '\u{2060}', "b"),
        ("x", '\u{200E}', "y"),
        ("ab", '\u{2066}', "cd"),
        ("ab", '\u{061C}', "cd"),
        ("ab", '\u{200B}', "cd"),
        ("ab", '\u{FEFF}', "cd"),
        ("ab", '\u{180E}', "cd"),
        ("\u{0628}", '\u{180E}', "\u{0628}"),
        ("ab", '\u{13430}', "cd"),
        ("ab", '\u{1BCA0}', "cd"),
        ("ab", '\u{FFF9}', "cd"),
        ("ab", '\u{FFFA}', "cd"),
        ("ab", '\u{1D173}', "cd"),
        ("ab", '\u{E0001}', "cd"),
        ("\u{0628}", '\u{200D}', "x"),
        ("x", '\u{200C}', "\u{0628}"),
    ] {
        let text = format!("{left}{invisible}{right}");
        let at = left.len();
        let after = at + invisible.len_utf8();
        let mut separated = SmartSelection::new();
        separated.set_word_separators(Some(" "));
        for smart in [&SmartSelection::new(), &separated] {
            assert_eq!(
                smart.word_boundaries_at(&text, 0),
                Some((0, at)),
                "{text:?}"
            );
            assert_eq!(smart.word_boundaries_at(&text, at), None, "{text:?}");
            assert_eq!(
                smart.word_boundaries_at(&text, after),
                Some((after, text.len())),
                "{text:?}"
            );
        }
    }
    for text in ["\u{0915}\u{094D}\u{200D} x", "x \u{200C}\u{0915}"] {
        let joiner = text.find(['\u{200C}', '\u{200D}']).unwrap();
        assert_eq!(
            SmartSelection::basic_word_boundaries(text, joiner),
            None,
            "{text:?}"
        );
    }
    let text = "\u{0915}\u{094D}\u{200D} x";
    assert_eq!(SmartSelection::basic_word_boundaries(text, 0), Some((0, 6)));
}

#[test]
fn a_prepended_mark_belongs_to_the_word_after_it() {
    let words = [
        "\u{06DD}\u{0661}\u{0662}\u{0663}",
        "\u{0600}123",
        "\u{070F}\u{0721}\u{072A}\u{0722}",
        "\u{110BD}123",
    ];
    let mut separated = SmartSelection::new();
    separated.set_word_separators(Some(" "));
    for word in words {
        let text = format!("x {word} y");
        for smart in [
            &SmartSelection::new(),
            &SmartSelection::with_builtin_rules(),
            &separated,
        ] {
            for column in 2..5 {
                assert_eq!(
                    smart.word_boundaries_at_column(&text, column),
                    Some((2, 5)),
                    "{word:?} clicked at column {column}"
                );
            }
        }
    }
}

#[test]
fn word_columns_follow_the_grids_fold_across_clusters() {
    let yansaya = "a \u{0D9A}\u{0DCA}\u{200D}\u{0DBA} z";
    let ya = yansaya.find('\u{0DBA}').unwrap();
    assert_eq!(byte_span_to_columns(yansaya, ya, ya + 3), (2, 3));
    assert_eq!(column_to_byte_pos(yansaya, 4), yansaya.len() - 1, "`z`");
    let text = "a \u{0D9A}\u{0DCA}\u{200D}\u{0DBA}\u{0DCF} z";
    assert_eq!(column_to_byte_pos(text, 5), text.len() - 1, "`z`");
    let smart = SmartSelection::default();
    assert_eq!(smart.word_boundaries_at_column(text, 5), Some((5, 6)));
}

#[test]
fn test_word_boundaries_at_column_cjk_with_space() {
    let text = "日本 test";
    let smart = SmartSelection::default();

    let bounds = smart.word_boundaries_at_column(text, 5);
    assert!(bounds.is_some(), "should find word at column 5");
    let (start_col, end_col) = bounds.unwrap();
    assert_eq!(start_col, 5, "word start column");
    assert_eq!(end_col, 9, "word end column");

    let bounds = smart.word_boundaries_at_column(text, 0);
    assert!(bounds.is_some(), "should find CJK word at column 0");
    let (start_col, end_col) = bounds.unwrap();
    assert_eq!(start_col, 0, "CJK word start column");
    assert_eq!(end_col, 4, "CJK word end column");
}

#[test]
fn test_word_boundaries_at_column_past_the_text_is_none() {
    let smart = SmartSelection::with_builtin_rules();
    let text = "see https://example.com";
    assert_eq!(smart.word_boundaries_at_column(text, 5), Some((4, 23)));
    assert_eq!(smart.word_boundaries_at_column(text, 22), Some((4, 23)));
    for column in [23, 24, 59] {
        assert_eq!(
            smart.word_boundaries_at_column(text, column),
            None,
            "column {column}"
        );
    }
    assert_eq!(smart.word_boundaries_at_column("", 0), None);
    assert_eq!(smart.word_boundaries_at_column("日本", 3), Some((0, 4)));
    assert_eq!(smart.word_boundaries_at_column("日本", 4), None);
}

#[test]
fn word_separators_make_a_word_any_run_of_non_separators() {
    let mut smart = SmartSelection::new();
    smart.set_word_separators(Some(" "));
    assert_eq!(smart.word_boundaries_at("foo-bar baz", 1), Some((0, 7)));
    assert_eq!(smart.word_boundaries_at("foo-bar baz", 3), Some((0, 7)));
    smart.set_word_separators(Some(" -"));
    assert_eq!(smart.word_boundaries_at("foo-bar baz", 1), Some((0, 3)));
    assert_eq!(smart.word_boundaries_at("foo-bar baz", 3), None);
    assert_eq!(smart.word_boundaries_at("foo-bar baz", 7), None);
}

#[test]
fn word_separators_keep_combining_marks_and_zero_width_breakers() {
    let mut smart = SmartSelection::new();
    smart.set_word_separators(Some(" "));
    let text = "e\u{301}f g";
    assert_eq!(smart.word_boundaries_at(text, 0), Some((0, 4)));
    let text = "ab\u{200B}cd";
    assert_eq!(smart.word_boundaries_at(text, 0), Some((0, 2)));
    let text = "\u{301}\u{301} x";
    assert_eq!(smart.word_boundaries_at(text, 0), None);
}

#[test]
fn word_separators_do_not_override_smart_rules() {
    let mut smart = SmartSelection::with_builtin_rules();
    smart.set_word_separators(Some(" /"));
    let text = "see https://example.com/path here";
    let bounds = smart
        .word_boundaries_at(text, 12)
        .expect("url rule matches");
    assert_eq!(&text[bounds.0..bounds.1], "https://example.com/path");
}
