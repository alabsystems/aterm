// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! UAX #29 Grapheme Cluster Break conformance tests.
//!
//! This test suite runs our grapheme segmenter against the official
//! `GraphemeBreakTest.txt` file from the Unicode Character Database, plus the
//! answers `unicode-segmentation` gave where it disagreed (recorded once, while
//! winit's Windows backend still used it).
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

/// `grapheme_indices()` offsets are the running byte offsets of `graphemes()`.
#[test]
fn grapheme_indices_agree_with_graphemes() {
    for input in [
        "abc",
        "café",
        "你好",
        "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}",
        "\u{1F1FA}\u{1F1F8}hello\u{1F1EC}\u{1F1E7}",
        "line1\r\nline2\n",
        "",
    ] {
        let mut at = 0usize;
        let expected: Vec<(usize, &str)> = GraphemeClusters::graphemes(input)
            .map(|g| {
                let entry = (at, g);
                at += g.len();
                entry
            })
            .collect();
        assert_eq!(at, input.len(), "graphemes cover {input:?}");
        let ours: Vec<(usize, &str)> = GraphemeClusters::grapheme_indices(input).collect();
        assert_eq!(ours, expected, "grapheme_indices for {input:?}");
    }
}

/// winit's Windows backend asks one question of a key event's text, "is it
/// more than one grapheme cluster?", and until 2026-10-15 asked it of
/// unicode-segmentation 1.13.3 (`grapheme_indices(true).count() > 1`, Unicode
/// 17 tables). It asks this crate now (`graphemes().count() > 1`, Unicode 16).
///
/// Recorded once with both crates, before the switch: the answers agree on
/// 1,092 of the 1,093 cases in GraphemeBreakTest-16.0.0.txt, on every scalar
/// value alone (1,112,064 strings), and on every scalar followed by each of
/// sixteen probes (U+0301, ZWJ, U+0903, a regional indicator, Hangul L, V, T and
/// LV, U+0600, `a`, CR, LF, VS16, a skin tone, virama, KA: 17,793,024 strings)
/// but eight. All nine disagreements are below; each is a Unicode 16 -> 17
/// property change, and this crate answers as the 16.0.0 file does. The
/// agreements are `uax29_official_conformance`.
#[test]
fn where_unicode_segmentation_disagreed_this_crate_follows_unicode_16() {
    // GraphemeBreakTest-16.0.0.txt line 1105: U+2701 is Extended_Pictographic
    // in Unicode 16 (GB11 joins across the ZWJ); unicode-segmentation's tables
    // drop it, and it answered two clusters.
    assert_eq!("\u{2701}\u{200D}\u{2701}".graphemes().count(), 1);
    // U+11A3A is Prepend in Unicode 16 (GB9b); unicode-segmentation's tables
    // have it as Other, and it answered two clusters before every probe that
    // starts a cluster of its own.
    for next in [
        'a',
        '\u{0600}',
        '\u{0915}',
        '\u{1100}',
        '\u{1161}',
        '\u{11A8}',
        '\u{AC00}',
        '\u{1F1E6}',
    ] {
        let text: String = ['\u{11A3A}', next].into_iter().collect();
        assert_eq!(
            text.graphemes().count(),
            1,
            "U+11A3A then U+{:04X}",
            next as u32
        );
    }
}
