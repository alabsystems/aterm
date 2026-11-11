// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Byte-level UTF-8 whitespace trimming and substring search matching standard `str` methods.

fn space_at(b: &[u8], i: usize) -> usize {
    match b[i] {
        b' ' | b'\t'..=b'\r' => 1,
        c if !matches!(c, 0xC2 | 0xE1 | 0xE2 | 0xE3) => 0,
        c => {
            let next = (b.get(i + 1).copied(), b.get(i + 2).copied());
            match (c, next) {
                (0xC2, (Some(0x85 | 0xA0), _)) => 2,
                (0xE1, (Some(0x9A), Some(0x80)))
                | (0xE2, (Some(0x80), Some(0x80..=0x8A | 0xA8 | 0xA9 | 0xAF)))
                | (0xE2, (Some(0x81), Some(0x9F)))
                | (0xE3, (Some(0x80), Some(0x80))) => 3,
                _ => 0,
            }
        }
    }
}

fn space_before(b: &[u8], j: usize) -> usize {
    let last = b[j - 1];
    if last.is_ascii() {
        return usize::from(matches!(last, b' ' | b'\t'..=b'\r'));
    }
    if j >= 2 && b[j - 2] == 0xC2 && matches!(last, 0x85 | 0xA0) {
        return 2;
    }
    if j >= 3 {
        let three = (b[j - 3], b[j - 2], last);
        if matches!(
            three,
            (0xE1, 0x9A, 0x80)
                | (0xE2, 0x80, 0x80..=0x8A | 0xA8 | 0xA9 | 0xAF)
                | (0xE2, 0x81, 0x9F)
                | (0xE3, 0x80, 0x80)
        ) {
            return 3;
        }
    }
    0
}

/// `s.trim_start()`.
pub(crate) fn trim_start(s: &str) -> &str {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b' ' {
            i += 1;
            continue;
        }
        match space_at(b, i) {
            0 => break,
            n => i += n,
        }
    }
    &s[i..]
}

/// `s.trim_end()`.
pub(crate) fn trim_end(s: &str) -> &str {
    let b = s.as_bytes();
    let mut j = b.len();
    while j > 0 {
        if b[j - 1] == b' ' {
            j -= 1;
            continue;
        }
        match space_before(b, j) {
            0 => break,
            n => j -= n,
        }
    }
    &s[..j]
}

/// `s.trim()`.
pub(crate) fn trim(s: &str) -> &str {
    trim_end(trim_start(s))
}

/// `s.trim().is_empty()`: a blank row.
pub(crate) fn blank(s: &str) -> bool {
    trim(s).is_empty()
}

/// Byte index of the first occurrence of `needle` in `s`, matching `s.find(needle)`. A
/// place is compared whole only when its first and last bytes match the needle's, so a
/// needle that opens on a space does not compare at every space of a row.
pub(crate) fn find(s: &str, needle: &str) -> Option<usize> {
    let (b, n) = (s.as_bytes(), needle.as_bytes());
    let (Some(&first), Some(&last)) = (n.first(), n.last()) else {
        return Some(0);
    };
    let mut i = 0;
    while i + n.len() <= b.len() {
        if b[i] == first && b[i + n.len() - 1] == last && &b[i..i + n.len()] == n {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// `s.contains(needle)` ([`find`]).
pub(crate) fn contains(s: &str, needle: &str) -> bool {
    find(s, needle).is_some()
}

/// Count of leading whitespace characters in `s`.
pub(crate) fn leading_space_chars(s: &str) -> usize {
    let b = s.as_bytes();
    let (mut i, mut chars) = (0, 0);
    while i < b.len() {
        match space_at(b, i) {
            0 => break,
            n => (i, chars) = (i + n, chars + 1),
        }
    }
    chars
}

/// Every fixture file under `src/fixtures/` as `(name, text)` in name order.
#[cfg(test)]
pub(crate) fn fixture_files() -> Vec<(String, String)> {
    let dir = aterm_tempfile::manifest_dir().join("src/fixtures");
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .expect("the fixtures")
        .map(|e| e.expect("a fixture").path())
        .collect();
    paths.sort();
    paths
        .iter()
        .map(|p| {
            let name = p
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            (name, std::fs::read_to_string(p).expect("a fixture"))
        })
        .collect()
}

/// Fixture screens and anchor test rows for byte-vs-char equivalence checks.
#[cfg(test)]
pub(crate) fn fixture_screens() -> Vec<Vec<String>> {
    let mut screens: Vec<Vec<String>> = fixture_files()
        .iter()
        .map(|(_, text)| crate::prompt::fixtures::screen(text))
        .collect();
    screens.push(
        [
            "─",
            "❯",
            " ❯",
            "❯ 1.",
            " ❯ 1. Yes",
            "❯❯ 2. x",
            "\u{a0}❯ 3.",
            "1.",
            "",
            "  ",
            "\t❯",
            "─────────────",
            "──────────────┬──",
            "── label ──────────  ",
            "› 1. Yes",
            "» x",
            "\u{2003}❯ draft",
            "│ ❯ quoted",
        ]
        .map(str::to_string)
        .to_vec(),
    );
    screens
}

#[cfg(test)]
#[path = "text_tests.rs"]
mod tests;
