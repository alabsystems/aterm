// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

/// A relative path with no `./` is matched WHOLE, from its first segment — never from
/// its first slash. The old pattern required a leading `/`, `./` or `../`, so the
/// leftmost match inside `crates/aterm-gui/src/lib.rs` was `/aterm-gui/src/lib.rs`: a
/// double-click copied an absolute path that does not exist.
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
    // A lone name with no slash is a word, not a path.
    assert_eq!(rule.find_all("just main.rs here").count(), 0);
}

#[test]
fn test_ipv4_pattern() {
    let rule = BuiltinRules::ipv4();
    let text = "Server at 192.168.1.100:8080 is up";
    let matches: Vec<_> = rule.find_all(text).collect();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].as_str(), "192.168.1.100:8080");
}

#[test]
fn test_git_hash_pattern() {
    let rule = BuiltinRules::git_hash();
    let text = "Commit abc1234 fixed the bug";
    let matches: Vec<_> = rule.find_all(text).collect();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].as_str(), "abc1234");
}

#[test]
fn test_quoted_string_with_escape() {
    let rule = BuiltinRules::double_quoted_string();
    let text = r#"echo "hello \"world\"" done"#;
    let matches: Vec<_> = rule.find_all(text).collect();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].as_str(), r#""hello \"world\"""#);
}

#[test]
fn uuid_outranks_git_hash_on_first_and_last_hex_groups() {
    // Regression: double-clicking (find_at) the 8-hex first group or the
    // 12-hex last group of a UUID must select the whole 36-char UUID, not
    // just the generic git_hash sub-segment. With uuid() at High priority it
    // now outranks git_hash in find_at, so clicking ANY hex group is consistent.
    let smart = SmartSelection::with_builtin_rules();
    let uuid = "550e8400-e29b-41d4-a716-446655440000";

    // Click inside the first hex group "550e8400" (bytes 0..8).
    let m = smart
        .find_at(uuid, 2)
        .expect("find_at inside first UUID group");
    assert_eq!(m.matched_text(), uuid, "first group selects whole UUID");
    assert_eq!((m.start(), m.end()), (0, uuid.len()));
    assert_eq!(m.kind(), SelectionRuleKind::Uuid);

    // Click inside the last hex group "446655440000" (bytes 24..36).
    let m = smart
        .find_at(uuid, 30)
        .expect("find_at inside last UUID group");
    assert_eq!(m.matched_text(), uuid, "last group selects whole UUID");
    assert_eq!((m.start(), m.end()), (0, uuid.len()));
    assert_eq!(m.kind(), SelectionRuleKind::Uuid);
}

#[test]
fn standalone_git_hash_still_matches_with_uuid_at_high() {
    // The uuid rule sitting at High priority must NOT starve a bare 40-hex
    // git hash: the UUID shape never matches plain hex, so git_hash still wins.
    let smart = SmartSelection::with_builtin_rules();
    let hash = "abcdef0123456789abcdef0123456789abcdef01";
    let m = smart.find_at(hash, 5).expect("find_at inside git hash");
    assert_eq!(m.matched_text(), hash);
    assert_eq!(m.kind(), SelectionRuleKind::GitHash);
}

#[test]
fn test_semver_pattern() {
    let rule = BuiltinRules::semver();
    let text = "Version v1.2.3-beta.1+build.456 released";
    let matches: Vec<_> = rule.find_all(text).collect();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].as_str(), "v1.2.3-beta.1+build.456");
}

// ── Negative tests: verify patterns do NOT match non-targets ──────

#[test]
fn url_rejects_plain_words() {
    let rule = BuiltinRules::url();
    for text in [
        "just some plain text",
        "not-a-url",
        "foo.bar.baz", // domain-like but no scheme
        "//no-scheme.example.com",
        "mailto:user@example.com", // different scheme class
    ] {
        let matches: Vec<_> = rule.find_all(text).collect();
        assert!(
            matches.is_empty(),
            "url() should NOT match {:?}, got {:?}",
            text,
            matches.iter().map(|m| m.as_str()).collect::<Vec<_>>()
        );
    }
}

#[test]
fn email_rejects_non_emails() {
    let rule = BuiltinRules::email();
    for text in [
        "not an email",
        "@missing-local.com",
        "user@",
        "user@.com",
        "user@com", // no dot in domain
        "user@@double.com",
    ] {
        let matches: Vec<_> = rule.find_all(text).collect();
        assert!(
            matches.is_empty(),
            "email() should NOT match {:?}, got {:?}",
            text,
            matches.iter().map(|m| m.as_str()).collect::<Vec<_>>()
        );
    }
}

#[test]
fn ipv4_rejects_non_ips() {
    let rule = BuiltinRules::ipv4();
    for text in [
        "999.999.999.999", // octets > 255
        "1.2.3",           // only 3 octets
        "not an ip address",
    ] {
        let matches: Vec<_> = rule.find_all(text).collect();
        assert!(
            matches.is_empty(),
            "ipv4() should NOT match {:?}, got {:?}",
            text,
            matches.iter().map(|m| m.as_str()).collect::<Vec<_>>()
        );
    }
}

#[test]
fn git_hash_rejects_short_hex() {
    let rule = BuiltinRules::git_hash();
    for text in [
        "abc123", // 6 chars — below 7 minimum
        "not hex at all",
        "12345", // 5 digits
    ] {
        let matches: Vec<_> = rule.find_all(text).collect();
        assert!(
            matches.is_empty(),
            "git_hash() should NOT match {:?}, got {:?}",
            text,
            matches.iter().map(|m| m.as_str()).collect::<Vec<_>>()
        );
    }
}

#[test]
fn uuid_rejects_malformed() {
    let rule = BuiltinRules::uuid();
    for text in [
        "not-a-uuid-at-all",
        "550e8400-e29b-41d4-a716",              // truncated
        "550e8400e29b41d4a716446655440000",     // no hyphens
        "ZZZZZZZZ-ZZZZ-ZZZZ-ZZZZ-ZZZZZZZZZZZZ", // non-hex
    ] {
        let matches: Vec<_> = rule.find_all(text).collect();
        assert!(
            matches.is_empty(),
            "uuid() should NOT match {:?}, got {:?}",
            text,
            matches.iter().map(|m| m.as_str()).collect::<Vec<_>>()
        );
    }
}

#[test]
fn file_path_rejects_plain_words() {
    let rule = BuiltinRules::file_path();
    for text in ["just a sentence", "no-slashes-here", "123.456"] {
        let matches: Vec<_> = rule.find_all(text).collect();
        assert!(
            matches.is_empty(),
            "file_path() should NOT match {:?}, got {:?}",
            text,
            matches.iter().map(|m| m.as_str()).collect::<Vec<_>>()
        );
    }
}

#[test]
fn test_basic_word_boundaries() {
    let text = "hello world_test foo";

    // Middle of "hello"
    let bounds = SmartSelection::basic_word_boundaries(text, 2);
    assert_eq!(bounds, Some((0, 5)));

    // Middle of "world_test"
    let bounds = SmartSelection::basic_word_boundaries(text, 8);
    assert_eq!(bounds, Some((6, 16)));

    // On space
    let bounds = SmartSelection::basic_word_boundaries(text, 5);
    assert_eq!(bounds, None);
}

#[test]
fn basic_word_boundaries_cjk() {
    // "日本語" = 3 chars, each 3 bytes UTF-8 = 9 bytes total
    let text = "日本語";
    let bounds = SmartSelection::basic_word_boundaries(text, 0);
    assert_eq!(bounds, Some((0, 9)));

    // Position in middle of CJK sequence (byte 3 = start of "本")
    let bounds = SmartSelection::basic_word_boundaries(text, 3);
    assert_eq!(bounds, Some((0, 9)));
}

#[test]
fn basic_word_boundaries_accented_latin() {
    // "café" = 'c'(1) + 'a'(1) + 'f'(1) + 'é'(2) = 5 bytes
    let text = "café";
    let bounds = SmartSelection::basic_word_boundaries(text, 0);
    assert_eq!(bounds, Some((0, 5)));

    // Position on 'f' (byte 2)
    let bounds = SmartSelection::basic_word_boundaries(text, 2);
    assert_eq!(bounds, Some((0, 5)));

    // Position on 'é' (byte 3, start of 2-byte char)
    let bounds = SmartSelection::basic_word_boundaries(text, 3);
    assert_eq!(bounds, Some((0, 5)));
}

#[test]
fn basic_word_boundaries_decomposed_accented_latin() {
    // "cafe\u{301}" = 'c' + 'a' + 'f' + 'e' + combining acute accent
    let text = "cafe\u{301}";

    // Position on base ASCII character.
    let bounds = SmartSelection::basic_word_boundaries(text, 0);
    assert_eq!(bounds, Some((0, text.len())));

    // Position on combining mark start.
    let bounds = SmartSelection::basic_word_boundaries(text, 4);
    assert_eq!(bounds, Some((0, text.len())));

    // Position in the middle of combining mark UTF-8 bytes snaps to mark start.
    let bounds = SmartSelection::basic_word_boundaries(text, 5);
    assert_eq!(bounds, Some((0, text.len())));
}

#[test]
fn basic_word_boundaries_combining_mark_without_base() {
    let text = "\u{0301}";
    let bounds = SmartSelection::basic_word_boundaries(text, 0);
    assert_eq!(bounds, None);
}

#[test]
fn basic_word_boundaries_hebrew_with_niqqud() {
    // "shalom" with niqqud combining marks.
    let text = "שָׁלוֹם";
    let bounds = SmartSelection::basic_word_boundaries(text, 0);
    assert_eq!(bounds, Some((0, text.len())));

    // Position on first niqqud mark (qamats).
    let qamats = text
        .find('\u{05B8}')
        .expect("qamats present in test string");
    let bounds = SmartSelection::basic_word_boundaries(text, qamats);
    assert_eq!(bounds, Some((0, text.len())));
}

#[test]
fn basic_word_boundaries_zero_width_space_splits_words() {
    let text = "foo\u{200B}bar";

    // Zero-width space should remain a boundary.
    let zwsp = text.find('\u{200B}').expect("zwsp present in test string");
    let bounds = SmartSelection::basic_word_boundaries(text, zwsp);
    assert_eq!(bounds, None);

    let bounds = SmartSelection::basic_word_boundaries(text, 0);
    assert_eq!(bounds, Some((0, 3)));

    let b_pos = text.find('b').expect("b present in test string");
    let bounds = SmartSelection::basic_word_boundaries(text, b_pos);
    assert_eq!(bounds, Some((b_pos, text.len())));
}

#[test]
fn basic_word_boundaries_bidi_controls_split_words() {
    for marker in [
        '\u{061C}', // Arabic Letter Mark
        '\u{200E}', // Left-to-Right Mark
        '\u{200F}', // Right-to-Left Mark
        '\u{202A}', // Left-to-Right Embedding
        '\u{202E}', // Right-to-Left Override
        '\u{2066}', // Left-to-Right Isolate
        '\u{2069}', // Pop Directional Isolate
    ] {
        let text = format!("foo{marker}bar");

        let marker_pos = text.find(marker).expect("marker present in test string");
        let bounds = SmartSelection::basic_word_boundaries(&text, marker_pos);
        assert_eq!(
            bounds, None,
            "marker U+{:04X} should split words",
            marker as u32
        );

        let bounds = SmartSelection::basic_word_boundaries(&text, 0);
        assert_eq!(
            bounds,
            Some((0, 3)),
            "marker U+{:04X} left segment",
            marker as u32
        );

        let b_pos = text.find('b').expect("b present in test string");
        let bounds = SmartSelection::basic_word_boundaries(&text, b_pos);
        assert_eq!(
            bounds,
            Some((b_pos, text.len())),
            "marker U+{:04X} right segment",
            marker as u32
        );
    }
}

/// Words a format control spells, each of which a double-click must select
/// whole: the Persian half space between two letters that would otherwise join,
/// a joiner next to a virama picking a conjunct's form, a script's own format
/// controls between two of its letters, and (no control) a virama and a
/// prepended number sign.
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

/// A ZWNJ or ZWJ that spells a word belongs to it. UAX #29 never breaks a word
/// at one (WB4), and the scripts that use them need them inside words: Persian
/// writes `می‌خواهم` with a half space, Marathi spells eyelash ra and Sinhala
/// `ශ්‍රී` with a ZWJ after the virama, Bengali spells ya-phala `র‍্য` with a ZWJ
/// before it. Every byte and every column of each word selects all of it, under
/// the default classifier, the builtin rules and configured separators alike.
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

/// An invisible character between two words still separates them: a joiner
/// between two letters that never join (`foo`‍`bar`, where it does no work), a
/// bidi override, isolate or mark, the word joiner, ZWSP and BOM. Invisible
/// characters must not weld two words together. A joiner at either end of a
/// run belongs to no word, and a click on any of them selects nothing.
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
        // A script's own format control between letters of another script (the
        // Mongolian vowel separator was a space before Unicode 6.3), and the
        // controls that spell no word: interlinear annotation, a musical beam,
        // the language tag.
        ("ab", '\u{180E}', "cd"),
        ("\u{0628}", '\u{180E}', "\u{0628}"),
        ("ab", '\u{13430}', "cd"),
        ("ab", '\u{1BCA0}', "cd"),
        ("ab", '\u{FFF9}', "cd"),
        ("ab", '\u{FFFA}', "cd"),
        ("ab", '\u{1D173}', "cd"),
        ("ab", '\u{E0001}', "cd"),
        // Two Arabic letters a ZWJ joins anyway, and a Latin letter beside one.
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
    // A joiner that ends or starts a run: next to a virama, but with no word
    // character on its other side.
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

/// A prepended concatenation mark (`Grapheme_Cluster_Break=Prepend`: the
/// Arabic number sign, end of ayah, the Syriac abbreviation mark) opens the
/// grapheme cluster of the character AFTER it (GB9b), and UAX #29 counts it
/// part of that word. A click on the cluster's column lands on the mark's
/// byte, so the walk must take the mark into the word after it. Double-click
/// on any digit of `۝١٢٣` selects all three.
#[test]
fn a_prepended_mark_belongs_to_the_word_after_it() {
    let words = [
        "\u{06DD}\u{0661}\u{0662}\u{0663}", // ۝١٢٣, end of ayah
        "\u{0600}123",                      // Arabic number sign
        "\u{070F}\u{0721}\u{072A}\u{0722}", // ܏ܡܪܢ, Syriac abbreviation mark
        "\u{110BD}123",                     // Kaithi number sign
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
fn basic_word_boundaries_cyrillic() {
    // "Привет" = 6 chars, each 2 bytes UTF-8 = 12 bytes
    let text = "Привет";
    let bounds = SmartSelection::basic_word_boundaries(text, 0);
    assert_eq!(bounds, Some((0, 12)));
}

#[test]
fn basic_word_boundaries_mixed_unicode_and_ascii() {
    // "hello 日本語 world"
    let text = "hello 日本語 world";
    // "hello" = bytes 0..5
    let bounds = SmartSelection::basic_word_boundaries(text, 0);
    assert_eq!(bounds, Some((0, 5)));

    // space at byte 5 → None
    let bounds = SmartSelection::basic_word_boundaries(text, 5);
    assert_eq!(bounds, None);

    // "日本語" starts at byte 6, each CJK char is 3 bytes → 6..15
    let bounds = SmartSelection::basic_word_boundaries(text, 6);
    assert_eq!(bounds, Some((6, 15)));

    // "world" starts at byte 16 → 16..21
    let bounds = SmartSelection::basic_word_boundaries(text, 16);
    assert_eq!(bounds, Some((16, 21)));
}

#[test]
fn basic_word_boundaries_mid_byte_position() {
    // If byte_pos lands in the middle of a multi-byte char, snap to its start
    // "é" is bytes [0xC3, 0xA9] — byte 1 is not a char boundary
    let text = "é";
    let bounds = SmartSelection::basic_word_boundaries(text, 1);
    assert_eq!(bounds, Some((0, 2)));
}

#[test]
fn test_column_to_byte_pos() {
    let text = "hello";
    assert_eq!(column_to_byte_pos(text, 0), 0);
    assert_eq!(column_to_byte_pos(text, 2), 2);
    assert_eq!(column_to_byte_pos(text, 5), 5);
    assert_eq!(column_to_byte_pos(text, 10), 5); // past end

    // With multibyte chars
    let text = "helloworldZ"; // "wo" is "wo" in bytes
    assert_eq!(column_to_byte_pos(text, 0), 0);
    assert_eq!(column_to_byte_pos(text, 5), 5);
}

#[test]
fn column_to_byte_pos_uses_display_columns_for_wide_chars() {
    // "日" is 3 bytes UTF-8, 2 display columns
    // "本" is 3 bytes UTF-8, 2 display columns
    // "test" is 4 bytes, 4 display columns
    let text = "日本test";
    // char indices:    0="日"(bytes 0..3), 1="本"(bytes 3..6), 2='t'(6), 3='e'(7), ...
    // display columns: 0-1="日", 2-3="本", 4='t', 5='e', 6='s', 7='t'

    // Start of first wide grapheme.
    assert_eq!(column_to_byte_pos(text, 0), 0);

    // Second cell of first wide grapheme should still map to its start.
    assert_eq!(column_to_byte_pos(text, 1), 0);

    // Start of second wide grapheme.
    assert_eq!(column_to_byte_pos(text, 2), 3);

    // Inside second wide grapheme should map to its start.
    assert_eq!(column_to_byte_pos(text, 3), 3);

    // Start of ASCII suffix.
    assert_eq!(column_to_byte_pos(text, 4), 6);
}

#[test]
fn test_byte_span_to_columns_ascii() {
    let text = "hello world";
    assert_eq!(byte_span_to_columns(text, 0, 0), (0, 0));
    assert_eq!(byte_span_to_columns(text, 0, 5), (0, 5));
    assert_eq!(byte_span_to_columns(text, 6, 11), (6, 11));
}

#[test]
fn test_byte_span_to_columns_cjk() {
    let text = "日本test";
    // "日" = 3 bytes, width 2; "本" = 3 bytes, width 2; "test" = 4 bytes, width 4
    assert_eq!(byte_span_to_columns(text, 0, 3), (0, 2)); // "日"
    assert_eq!(byte_span_to_columns(text, 3, 6), (2, 4)); // "本"
    assert_eq!(byte_span_to_columns(text, 6, 10), (4, 8)); // "test"
}

/// Columns follow the grid's fold across cluster boundaries: Sinhala `ක්‍ය`
/// is two UAX #29 clusters the grid draws in ONE cell, so `ය` alone is drawn
/// in `ක`'s cell and `ක්‍යා` is two cells (`ක්‍ය`, `ා`): a double-click on the
/// `z` after it finds the `z`, not the space before it.
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
    // "日本 test": a column must not be read as a byte offset (#5685).
    let text = "日本 test";
    // Bytes:   0..3="日", 3..6="本", 6=" ", 7..11="test"
    // Columns: 0-1="日", 2-3="本", 4=" ", 5-8="test"
    let smart = SmartSelection::default();

    // Column 5 = start of "test"; byte 5 would be inside "本".
    let bounds = smart.word_boundaries_at_column(text, 5);
    assert!(bounds.is_some(), "should find word at column 5");
    let (start_col, end_col) = bounds.unwrap();
    assert_eq!(start_col, 5, "word start column");
    assert_eq!(end_col, 9, "word end column");

    // Column 0 = start of CJK word "日本".
    let bounds = smart.word_boundaries_at_column(text, 0);
    assert!(bounds.is_some(), "should find CJK word at column 0");
    let (start_col, end_col) = bounds.unwrap();
    assert_eq!(start_col, 0, "CJK word start column");
    assert_eq!(end_col, 4, "CJK word end column");
}

/// A column at or past the end of the text names an empty cell, not the last
/// word. The column→byte step maps every such column to `text.len()`, and the
/// byte walk clamped that onto the last character, so the web build's
/// double-click (`Terminal::smart_word_at`) on the blank cells right of
/// `https://example.com` selected `com`.
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
    // `日本` occupies columns 0..4: column 3 is its last cell, 4 is past it.
    assert_eq!(smart.word_boundaries_at_column("日本", 3), Some((0, 4)));
    assert_eq!(smart.word_boundaries_at_column("日本", 4), None);
}

#[test]
fn test_word_boundaries_at_column_ascii_matches_byte() {
    // For pure ASCII, column and byte positions are identical.
    let text = "hello world";
    let smart = SmartSelection::default();

    let byte_bounds = smart.word_boundaries_at(text, 0);
    let col_bounds = smart.word_boundaries_at_column(text, 0);
    assert_eq!(
        byte_bounds, col_bounds,
        "ASCII: byte and column results should match"
    );
}

// ---- Configurable word separators (xterm.js `wordSeparators`) --------------

#[test]
fn word_separators_default_is_class_based_and_unset_preserves_it() {
    // No rules: the walk goes straight to the basic word fallback.
    let mut smart = SmartSelection::new();
    // Default: '-' is not a word char, so "foo-bar" splits at the hyphen.
    assert_eq!(smart.word_separators(), None);
    assert_eq!(smart.word_boundaries_at("foo-bar baz", 1), Some((0, 3)));
    // Setting then CLEARING the separators restores the default exactly.
    smart.set_word_separators(Some(" "));
    smart.set_word_separators(None);
    assert_eq!(smart.word_separators(), None);
    assert_eq!(smart.word_boundaries_at("foo-bar baz", 1), Some((0, 3)));
}

#[test]
fn word_separators_make_a_word_any_run_of_non_separators() {
    let mut smart = SmartSelection::new();
    // Space-only separators: the hyphenated token is ONE word now.
    smart.set_word_separators(Some(" "));
    assert_eq!(smart.word_separators(), Some(" "));
    assert_eq!(smart.word_boundaries_at("foo-bar baz", 1), Some((0, 7)));
    // Clicking the hyphen itself lands inside the same word.
    assert_eq!(smart.word_boundaries_at("foo-bar baz", 3), Some((0, 7)));
    // Adding '-' back to the set restores the split.
    smart.set_word_separators(Some(" -"));
    assert_eq!(smart.word_boundaries_at("foo-bar baz", 1), Some((0, 3)));
    // Clicking ON a separator is not a word (the caller falls back to the cell).
    assert_eq!(smart.word_boundaries_at("foo-bar baz", 3), None);
    assert_eq!(smart.word_boundaries_at("foo-bar baz", 7), None);
}

#[test]
fn word_separators_keep_combining_marks_and_zero_width_breakers() {
    let mut smart = SmartSelection::new();
    smart.set_word_separators(Some(" "));
    // A combining mark extends the run onto its base ("e\u{301}f" is one word).
    let text = "e\u{301}f g";
    assert_eq!(smart.word_boundaries_at(text, 0), Some((0, 4)));
    // A zero-width space still BREAKS even though it is not in the set —
    // invisible characters must not weld two words together.
    let text = "ab\u{200B}cd";
    assert_eq!(smart.word_boundaries_at(text, 0), Some((0, 2)));
    // And a run of pure combining marks is still not a word.
    let text = "\u{301}\u{301} x";
    assert_eq!(smart.word_boundaries_at(text, 0), None);
}

#[test]
fn word_separators_do_not_override_smart_rules() {
    // Rules still take precedence: even with '/' as a separator, clicking
    // inside a URL selects the WHOLE url via the builtin rule, not the
    // separator-shaped fragment.
    let mut smart = SmartSelection::with_builtin_rules();
    smart.set_word_separators(Some(" /"));
    let text = "see https://example.com/path here";
    let bounds = smart
        .word_boundaries_at(text, 12)
        .expect("url rule matches");
    assert_eq!(&text[bounds.0..bounds.1], "https://example.com/path");
}
