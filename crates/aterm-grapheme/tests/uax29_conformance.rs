// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! UAX #29 Grapheme Cluster Break conformance tests.
//!
//! This test suite runs our grapheme segmenter against the official
//! `GraphemeBreakTest.txt` file from the Unicode Character Database, plus
//! a cross-validation pass against the `unicode-segmentation` crate on a
//! large corpus of realistic terminal text.
//!
//! The UAX #29 test file uses the following format:
//! ```text
//! ÷ 0020 × 0308 ÷ 0020 ÷    # <comment>
//! ```
//! where ÷ marks a break opportunity and × marks a no-break. Lines beginning
//! with `#` are comments and ignored.

use aterm_grapheme::GraphemeClusters;

/// A single UAX #29 conformance test case.
#[derive(Debug)]
struct TestCase {
    line_number: usize,
    /// The expected sequence of grapheme clusters, each as a Vec of codepoints.
    expected: Vec<Vec<u32>>,
    /// The full concatenated text, used to drive the segmenter.
    input: String,
    /// The original line for error messages.
    raw: String,
}

/// Parse a single UAX #29 test line into a `TestCase`.
///
/// Returns `None` for comment/blank lines.
fn parse_line(line: &str, line_number: usize) -> Option<TestCase> {
    let raw = line.to_string();
    // Strip comment.
    let content = line.split('#').next().unwrap_or("").trim();
    if content.is_empty() {
        return None;
    }

    // Tokenize on whitespace. Tokens are either ÷ (U+00F7), × (U+00D7), or
    // 4-6 hex-digit codepoints.
    let mut expected: Vec<Vec<u32>> = Vec::new();
    let mut current: Vec<u32> = Vec::new();
    let mut saw_first_break = false;

    for tok in content.split_whitespace() {
        match tok {
            "÷" => {
                if saw_first_break && !current.is_empty() {
                    expected.push(std::mem::take(&mut current));
                }
                saw_first_break = true;
            }
            "×" => {
                // No-break: continue accumulating in `current`.
            }
            hex => {
                let cp = u32::from_str_radix(hex, 16).ok()?;
                current.push(cp);
            }
        }
    }
    // If the last token was ÷ and current has been drained, nothing to do.
    // Otherwise push trailing cluster.
    if !current.is_empty() {
        expected.push(current);
    }

    // Build the concatenated input string.
    let mut input = String::new();
    for cluster in &expected {
        for &cp in cluster {
            input.push(char::from_u32(cp)?);
        }
    }

    Some(TestCase {
        line_number,
        expected,
        input,
        raw,
    })
}

/// Segment `input` and compare against `expected` (a Vec of cluster codepoint
/// sequences).
///
/// Returns `Ok(())` on match, otherwise an `Err` with a diagnostic.
fn check(tc: &TestCase) -> Result<(), String> {
    let got: Vec<Vec<u32>> = tc
        .input
        .graphemes()
        .map(|g| g.chars().map(|c| c as u32).collect())
        .collect();

    if got == tc.expected {
        Ok(())
    } else {
        Err(format!(
            "line {}: mismatch\n  raw:      {}\n  expected: {:?}\n  got:      {:?}",
            tc.line_number,
            tc.raw.trim(),
            tc.expected,
            got
        ))
    }
}

#[test]
fn uax29_official_conformance() {
    let data = include_str!("data/GraphemeBreakTest-16.0.0.txt");

    let mut total = 0usize;
    let mut passed = 0usize;
    let mut failures: Vec<String> = Vec::new();

    for (idx, line) in data.lines().enumerate() {
        let line_number = idx + 1;
        let Some(tc) = parse_line(line, line_number) else {
            continue;
        };
        total += 1;
        match check(&tc) {
            Ok(()) => passed += 1,
            Err(msg) => failures.push(msg),
        }
    }

    if !failures.is_empty() {
        // Print up to 30 failures so the user can see patterns without
        // spamming the test log.
        let show = failures.len().min(30);
        let summary = format!(
            "UAX #29 conformance: {}/{} passed, {} failures (showing first {})\n{}",
            passed,
            total,
            failures.len(),
            show,
            failures
                .iter()
                .take(show)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n"),
        );
        panic!("{summary}");
    }

    assert!(
        total > 0,
        "no test cases parsed from GraphemeBreakTest-16.0.0.txt"
    );
    eprintln!("UAX #29 conformance: {passed}/{total} passed");
}

/// Corpus-based cross-validation against the `unicode-segmentation` crate.
///
/// The corpus covers the categories most relevant to terminal text: ASCII,
/// Latin-1 with combining marks, CJK, Indic scripts, emoji (simple, skin
/// tone, ZWJ sequences, flags, tag sequences), and control sequences.
#[test]
fn cross_validate_unicode_segmentation() {
    use unicode_segmentation::UnicodeSegmentation;

    let corpus = [
        // ASCII and Latin-1
        "Hello, world!",
        "café résumé naïve",
        "a\u{0308}\u{0301}b\u{0300}c",
        // CJK
        "你好世界",
        "日本語",
        "中文繁體",
        "한국어 문자",
        // Hangul Jamo decomposed
        "\u{1100}\u{1161}\u{11A8}", // 각
        "\u{1100}\u{1161}",         // 가
        "\u{1100}",                 // L alone
        // Indic scripts
        "\u{0915}\u{093E}\u{0928}\u{0940}", // Devanagari kaani
        "\u{0D15}\u{0D4D}\u{0D38}",         // Malayalam ക്സ (GB9c conjunct)
        "\u{0915}\u{094D}\u{092F}",         // Devanagari क्य (GB9c conjunct)
        "\u{0915}\u{094D}\u{0937}",         // Devanagari क्ष (GB9c conjunct)
        "\u{1780}\u{17B6}",                 // Khmer ka
        // Emoji: simple, skin tone, gender, ZWJ family, flag, tag flag.
        "\u{1F600}",
        "\u{1F44B}\u{1F3FD}",
        "\u{1F468}\u{200D}\u{1F52C}",
        "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}",
        "\u{1F1FA}\u{1F1F8}",
        "\u{1F1EB}\u{1F1F7}\u{1F1EC}\u{1F1E7}", // two flags in a row
        "\u{1F3F4}\u{E0067}\u{E0062}\u{E0065}\u{E006E}\u{E0067}\u{E007F}",
        // Keycap
        "1\u{FE0F}\u{20E3}",
        // Mixed complexity
        "Hello 🌍! 中文 + العربية‎ + कहानी",
        // Line endings
        "line1\r\nline2\nline3\r\n",
        // Control chars
        "a\x00b\x01c\x1Fd",
        // Variation selectors
        "A\u{FE0F}",
        "\u{1F441}\u{FE0F}",
        "\u{1F441}\u{FE0F}\u{200D}\u{1F5E8}",
        // RI triples
        "\u{1F1FA}\u{1F1F8}\u{1F1EC}",
        "\u{1F1FA}\u{1F1F8}\u{1F1EC}\u{1F1E7}\u{1F1EB}\u{1F1F7}",
        // Degenerate: empty, single codepoint, standalone combiners
        "",
        "x",
        "\u{0301}",
        "\u{200D}",
        "\u{200D}\u{1F600}",
    ];

    let mut mismatches = Vec::new();
    for input in &corpus {
        let ours: Vec<&str> = GraphemeClusters::graphemes(*input).collect();
        let theirs: Vec<&str> = UnicodeSegmentation::graphemes(*input, true).collect();
        if ours != theirs {
            mismatches.push(format!(
                "input {:?}\n  ours:   {:?}\n  theirs: {:?}",
                input, ours, theirs
            ));
        }
    }

    if !mismatches.is_empty() {
        panic!(
            "cross-validation mismatches with unicode-segmentation ({}/{}):\n{}",
            mismatches.len(),
            corpus.len(),
            mismatches.join("\n")
        );
    }
}

/// Cross-validate grapheme_indices() byte offsets.
#[test]
fn cross_validate_indices() {
    use unicode_segmentation::UnicodeSegmentation;

    let corpus = [
        "abc",
        "café",
        "你好",
        "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}",
        "\u{1F1FA}\u{1F1F8}hello\u{1F1EC}\u{1F1E7}",
        "line1\r\nline2\n",
    ];

    for input in &corpus {
        let ours: Vec<(usize, &str)> = GraphemeClusters::grapheme_indices(*input).collect();
        let theirs: Vec<(usize, &str)> =
            UnicodeSegmentation::grapheme_indices(*input, true).collect();
        assert_eq!(
            ours, theirs,
            "grapheme_indices mismatch for input {input:?}"
        );
    }
}
