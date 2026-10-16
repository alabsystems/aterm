// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `str::trim`, `str::trim_start` and `str::trim_end`, read on BYTES.
//!
//! Every reading walks every row of the screen many times over (each of the
//! reader's functions finds the composer, the rules and the box for itself),
//! and the char-wise trims, generic code a debug build compiles unoptimized,
//! were the reader's heaviest loop: the panic sweep spent more of its CPU in
//! them than in anything the reader decides. These return the SAME slices —
//! whitespace is `char::is_whitespace`, Unicode's White_Space, matched on its
//! UTF-8 bytes — and `the_byte_trims_are_the_char_trims` checks every char.

/// The length in bytes of the char that STARTS at byte `i` of `b` when it is
/// whitespace, else `0` (`i` on a char boundary, `i < b.len()`). White_Space
/// is U+0009..=U+000D, U+0020, U+0085, U+00A0, U+1680, U+2000..=U+200A,
/// U+2028, U+2029, U+202F, U+205F and U+3000.
fn space_at(b: &[u8], i: usize) -> usize {
    match b[i] {
        b' ' | b'\t'..=b'\r' => 1,
        // ASCII, or a lead byte no whitespace char opens with.
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

/// The length in bytes of the char that ENDS at byte `j` of `b` (exclusive)
/// when it is whitespace, else `0` (`j` on a char boundary, `j > 0`). The
/// bytes matched for a two- or three-byte char start with a lead byte, so
/// they are that char whole.
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

/// `s.find(needle)`: each byte that opens the needle compared, the rest
/// skipped one at a time. A match starts on the needle's first byte, which
/// opens a char, so it is the char boundary `str::find` names.
pub(crate) fn find(s: &str, needle: &str) -> Option<usize> {
    let (b, n) = (s.as_bytes(), needle.as_bytes());
    let Some(&first) = n.first() else {
        return Some(0);
    };
    let mut i = 0;
    while i + n.len() <= b.len() {
        if b[i] == first && &b[i..i + n.len()] == n {
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

/// How many chars `s.trim_start()` cuts: `s.chars().take_while(|c|
/// c.is_whitespace()).count()`.
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

/// Every fixture file under `src/fixtures/` as `(its file name, its text)`,
/// in name order: the tests that hold one program's reader to every other
/// program's screens read the program off the name (`agy-…`, `codex-…`).
#[cfg(test)]
pub(crate) fn fixture_files() -> Vec<(String, String)> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/fixtures");
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

/// The screens the byte readings are checked on against the char readings
/// they replace: every fixture file under `src/fixtures/`, as its rows, and
/// the rows the reader's anchors make alone.
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
mod tests {
    use super::*;

    /// Every char, alone and between others, trims, counts and is matched
    /// exactly as the char-wise functions do — whitespace or not, at the
    /// start, the end, and next to every whitespace char.
    #[test]
    fn the_byte_trims_are_the_char_trims() {
        let spaces: Vec<char> = (0..=0x10_FFFF_u32)
            .filter_map(char::from_u32)
            .filter(|c| c.is_whitespace())
            .collect();
        assert_eq!(spaces.len(), 25, "Unicode's White_Space: {spaces:?}");
        let check = |s: &str| {
            assert_eq!(trim_start(s), s.trim_start(), "{s:?}");
            assert_eq!(trim_end(s), s.trim_end(), "{s:?}");
            assert_eq!(trim(s), s.trim(), "{s:?}");
            assert_eq!(
                leading_space_chars(s),
                s.chars().take_while(|c| c.is_whitespace()).count(),
                "{s:?}"
            );
        };
        for c in (0..=0x10_FFFF_u32).filter_map(char::from_u32) {
            let mut buf = [0; 4];
            let one = c.encode_utf8(&mut buf);
            assert_eq!(
                space_at(one.as_bytes(), 0),
                if c.is_whitespace() { one.len() } else { 0 },
                "{c:?}"
            );
            assert_eq!(
                space_before(one.as_bytes(), one.len()),
                if c.is_whitespace() { one.len() } else { 0 },
                "{c:?}"
            );
            check(one);
        }
        // Runs of whitespace around every char whose bytes a whitespace
        // char's could be mistaken for: those sharing all but its last byte
        // (`C2 xx`, `E1 9A xx`, `E2 80 xx`, `E2 81 xx`, `E3 80 xx`).
        let near: Vec<char> = (0x80..=0x3100_u32)
            .filter_map(char::from_u32)
            .filter(|&c| {
                let (mut cb, mut sb) = ([0; 4], [0; 4]);
                let cb = c.encode_utf8(&mut cb).as_bytes();
                spaces.iter().any(|s| {
                    let sb = s.encode_utf8(&mut sb).as_bytes();
                    sb.len() > 1 && sb.len() == cb.len() && sb[..sb.len() - 1] == cb[..cb.len() - 1]
                })
            })
            .collect();
        for &a in &spaces {
            for &b in &spaces {
                check(&format!("{a}{b}x{a}y{b}{a}"));
            }
            for &c in &near {
                check(&format!("{a}{c}{a}"));
                check(&format!("{c}{a}{c}"));
            }
        }
        for s in [
            "",
            " ",
            "  ❯ 1. Yes  ",
            "\u{a0}❯\u{a0}",
            "─── ┬ ───",
            "⏺ said\t\n",
        ] {
            check(s);
        }
    }

    /// `find` and `contains` name the match `str::find` names, first of
    /// several, at either end, overlapping itself, or none.
    #[test]
    fn the_byte_search_is_the_str_search() {
        let hays = [
            "",
            "Esc to cancel · Tab to amend",
            "  Esc to Esc to ",
            "aaab",
            "❯ 1. Yes ❯",
            "3 shells · 2 shells ·",
            "·",
        ];
        let needles = [
            "",
            "Esc to ",
            "aab",
            "❯",
            "· ",
            " shells ·",
            "·",
            "zz",
            "Esc to cancel · Tab to amend!",
        ];
        for h in hays {
            for n in needles {
                assert_eq!(find(h, n), h.find(n), "{h:?} {n:?}");
                assert_eq!(contains(h, n), h.contains(n), "{h:?} {n:?}");
            }
        }
    }
}
