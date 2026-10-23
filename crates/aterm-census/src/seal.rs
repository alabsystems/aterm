// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Generated-file seals: the generator stamps each file it writes with a digest of that file,
//! so a hand edit of the file (a comment prune) shows without the generator having to run.
//!
//! A seal line sits in the file's first [`HEAD_LINES`] lines and carries both [`MARK`] and
//! `seal ` followed by 16 lowercase hex digits: the FNV-1a 64 digest of the whole file with
//! those 16 digits read as `0`. A generator writes the line with zeros ([`line`]), formats its
//! output if it formats, and then calls [`stamp`] on what it will write; the `generated-seal`
//! guard calls [`check`] on every file `.gitattributes` marks `linguist-generated`.

use std::ops::Range;

/// The word every seal line carries.
pub const MARK: &str = "@generated";

/// How many leading lines a seal line may sit in.
pub const HEAD_LINES: usize = 16;

/// The token before the digest.
const SEAL: &str = "seal ";

/// The digest's width in hex digits.
const DIGITS: usize = 16;

/// What a file's seal says about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Seal {
    /// No seal line in the file's head.
    Absent,
    /// The seal on `line` (1-based) matches the file.
    Holds { line: usize },
    /// The seal on `line` (1-based) records `sealed`, and the file now digests to `now`.
    Broken {
        line: usize,
        sealed: String,
        now: String,
    },
}

/// The unstamped seal line a generator writes: `comment` is the line's comment leader (`//`,
/// `#`), `generator` the command that writes the file.
#[must_use]
pub fn line(comment: &str, generator: &str) -> String {
    format!(
        "{comment} {MARK} by {generator} ({SEAL}{zeros}); rerun it, never edit by hand.",
        zeros = "0".repeat(DIGITS)
    )
}

/// `text` with its seal filled in.
///
/// # Errors
/// When `text` carries no seal line in its head.
pub fn stamp(text: &str) -> Result<String, String> {
    let (_, digits) = locate(text).ok_or_else(|| {
        format!("no `{MARK} … {SEAL}<{DIGITS} hex digits>` line in the first {HEAD_LINES} lines")
    })?;
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..digits.start]);
    out.push_str(&format!("{:016x}", digest(text, &digits)));
    out.push_str(&text[digits.end..]);
    Ok(out)
}

/// Does `text`'s seal hold?
#[must_use]
pub fn check(text: &str) -> Seal {
    let Some((line, digits)) = locate(text) else {
        return Seal::Absent;
    };
    let now = format!("{:016x}", digest(text, &digits));
    let sealed = &text[digits.clone()];
    if sealed == now {
        Seal::Holds { line }
    } else {
        Seal::Broken {
            line,
            sealed: sealed.to_string(),
            now,
        }
    }
}

/// The 1-based line of the head's first seal line, and the byte range of its digits.
fn locate(text: &str) -> Option<(usize, Range<usize>)> {
    let mut start = 0;
    for (index, raw) in text.split_inclusive('\n').take(HEAD_LINES).enumerate() {
        if raw.contains(MARK) {
            for (at, _) in raw.match_indices(SEAL) {
                let from = at + SEAL.len();
                let hex = raw.as_bytes().get(from..from + DIGITS);
                let after = raw.as_bytes().get(from + DIGITS);
                if hex.is_some_and(|h| h.iter().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
                    && !after.is_some_and(u8::is_ascii_hexdigit)
                {
                    return Some((index + 1, start + from..start + from + DIGITS));
                }
            }
        }
        start += raw.len();
    }
    None
}

/// FNV-1a 64 of `text` with the bytes at `digits` read as `0`.
fn digest(text: &str, digits: &Range<usize>) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for (at, byte) in text.bytes().enumerate() {
        let byte = if digits.contains(&at) { b'0' } else { byte };
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    fn generated() -> String {
        format!(
            "// Copyright 2026 Andrew Yates\n// SPDX-License-Identifier: Apache-2.0\n{}\n\n\
             // What the table holds.\npub const N: u32 = 1;\n",
            line("//", "`gen tables`")
        )
    }

    #[test]
    fn a_stamped_file_holds_and_any_edit_breaks_it() {
        let stamped = stamp(&generated()).expect("the head carries a seal line");
        assert_eq!(check(&stamped), Seal::Holds { line: 3 });
        assert_eq!(stamp(&stamped).as_deref(), Ok(stamped.as_str()));
        let pruned = stamped.replace("// What the table holds.\n", "");
        assert!(matches!(check(&pruned), Seal::Broken { line: 3, .. }));
        let retitled = stamped.replace("`gen tables`", "`gen`");
        assert!(matches!(check(&retitled), Seal::Broken { line: 3, .. }));
        assert!(matches!(check(&generated()), Seal::Broken { line: 3, .. }));
    }

    #[test]
    fn a_seal_is_read_only_from_the_head_and_only_beside_the_mark() {
        assert_eq!(check("fn main() {}\n"), Seal::Absent);
        assert_eq!(check("// seal 0000000000000000\n"), Seal::Absent);
        assert_eq!(
            check("// @generated seal 00000000000000000\n"),
            Seal::Absent
        );
        let late = format!("{}{}\n", "//\n".repeat(HEAD_LINES), line("//", "x"));
        assert_eq!(check(&late), Seal::Absent);
        assert!(stamp(&late).is_err());
    }
}
