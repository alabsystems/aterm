// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! UAX #29 Grapheme Cluster Break conformance tests.

use aterm_grapheme::GraphemeClusters;

/// A single UAX #29 conformance test case.
#[derive(Debug)]
struct TestCase {
    line_number: usize,
    expected: Vec<Vec<u32>>,
    input: String,
    raw: String,
}

fn parse_line(line: &str, line_number: usize) -> Option<TestCase> {
    let raw = line.to_string();
    let content = line.split('#').next().unwrap_or("").trim();
    if content.is_empty() {
        return None;
    }

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
    if !current.is_empty() {
        expected.push(current);
    }

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

#[test]
fn where_unicode_segmentation_disagreed_this_crate_follows_unicode_16() {
    assert_eq!("\u{2701}\u{200D}\u{2701}".graphemes().count(), 1);
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
