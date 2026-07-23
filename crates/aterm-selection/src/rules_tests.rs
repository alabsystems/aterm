// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

#[test]
fn test_url_pattern() {
    let rule = BuiltinRules::url();
    let text = "Check https://example.com/path?q=1 for info";
    let matches: Vec<_> = rule.find_all(text).collect();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].as_str(), "https://example.com/path?q=1");
}

#[test]
fn test_url_with_trailing_punctuation() {
    let rule = BuiltinRules::url();
    let text = "See https://example.com.";
    let matches: Vec<_> = rule.find_all(text).collect();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].as_str(), "https://example.com");
}

#[test]
fn test_file_path_unix() {
    let rule = BuiltinRules::file_path();
    let text = "File at ./file.txt exists";
    let matches: Vec<_> = rule.find_all(text).collect();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].as_str(), "./file.txt");
}

#[test]
fn test_file_path_relative() {
    let rule = BuiltinRules::file_path();
    let text = "Check ./src/main.rs file";
    let matches: Vec<_> = rule.find_all(text).collect();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].as_str(), "./src/main.rs");
}

#[test]
fn test_email_pattern() {
    let rule = BuiltinRules::email();
    let text = "Contact user@example.com for info";
    let matches: Vec<_> = rule.find_all(text).collect();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].as_str(), "user@example.com");
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
fn test_git_hash_full() {
    let rule = BuiltinRules::git_hash();
    let text = "SHA: abcdef0123456789abcdef0123456789abcdef01";
    let matches: Vec<_> = rule.find_all(text).collect();
    assert_eq!(matches.len(), 1);
    assert_eq!(
        matches[0].as_str(),
        "abcdef0123456789abcdef0123456789abcdef01"
    );
}

#[test]
fn test_quoted_string_double() {
    let rule = BuiltinRules::double_quoted_string();
    let text = r#"echo "hello world" done"#;
    let matches: Vec<_> = rule.find_all(text).collect();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].as_str(), r#""hello world""#);
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
fn test_uuid_pattern() {
    let rule = BuiltinRules::uuid();
    let text = "ID: 550e8400-e29b-41d4-a716-446655440000 found";
    let matches: Vec<_> = rule.find_all(text).collect();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].as_str(), "550e8400-e29b-41d4-a716-446655440000");
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
fn test_byte_pos_to_column_ascii() {
    let text = "hello world";
    assert_eq!(byte_pos_to_column(text, 0), 0);
    assert_eq!(byte_pos_to_column(text, 5), 5);
    assert_eq!(byte_pos_to_column(text, 11), 11);
}

#[test]
fn test_byte_pos_to_column_cjk() {
    let text = "日本test";
    // "日" = 3 bytes, width 2; "本" = 3 bytes, width 2; "test" = 4 bytes, width 4
    assert_eq!(byte_pos_to_column(text, 0), 0); // start of "日"
    assert_eq!(byte_pos_to_column(text, 3), 2); // start of "本"
    assert_eq!(byte_pos_to_column(text, 6), 4); // start of "t"
    assert_eq!(byte_pos_to_column(text, 10), 8); // end
}

#[test]
fn test_word_boundaries_at_column_cjk_with_space() {
    // Regression test for #5685: smart_word_at passes column as byte position.
    // "日本 test" — CJK word, space, ASCII word.
    let text = "日本 test";
    // Bytes:   0..3="日", 3..6="本", 6=" ", 7..11="test"
    // Columns: 0-1="日", 2-3="本", 4=" ", 5-8="test"
    let smart = SmartSelection::default();

    // Column 5 = start of "test" (after CJK + space).
    // Without the fix, column 5 was passed as byte position 5 (inside "本").
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

#[test]
fn test_word_boundaries_at_column_cjk_regression_5685() {
    // Regression proof for #5685: demonstrates the bug by comparing old API
    // (word_boundaries_at, byte-indexed) vs new API (word_boundaries_at_column,
    // column-indexed) when given a column index on CJK text.
    //
    // "日本 test" — CJK chars are 3 bytes each, 2 display columns each.
    // Bytes:   0..3="日", 3..6="本", 6=" ", 7..11="test"
    // Columns: 0-1="日", 2-3="本", 4=" ", 5-8="test"
    let text = "日本 test";
    let smart = SmartSelection::default();

    // A terminal UI asks "what word is at column 5?" (the "t" in "test").
    let column = 5_usize;

    // OLD API: treats column as byte position → byte 5 is inside "本" (bytes 3..6).
    // This returns the CJK word "日本" in byte offsets — completely wrong for
    // a caller that meant column 5.
    let old_result = smart.word_boundaries_at(text, column);
    assert_eq!(old_result, Some((0, 6)), "old API: byte 5 → CJK word bytes");

    // NEW API: converts column→byte first, then byte→column on output.
    // Returns the correct word "test" in column coordinates.
    let new_result = smart.word_boundaries_at_column(text, column);
    assert_eq!(
        new_result,
        Some((5, 9)),
        "new API: column 5 → 'test' columns"
    );

    // The two results are DIFFERENT — this proves the bug existed.
    assert_ne!(
        old_result, new_result,
        "CJK text: byte-indexed and column-indexed results must differ"
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
