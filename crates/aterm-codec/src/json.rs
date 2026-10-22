// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The JSON string escape every control-path JSON writer shares: the control
//! wire's `--json` replies, `subscribe … cells` and the asciicast emitter
//! (`aterm_control::wire::json_escape*` re-export it), and the supervisor's
//! journal.
//!
//! RFC 8259 requires only `"`, `\` and U+0000-U+001F escaped, and a writer
//! that stops there is valid JSON — but a reply is often read raw on a
//! terminal, and a value in it is text a program chose (an OSC 7 cwd, an OSC
//! 633 command line, a cell's glyph). So every character a terminal acts on or
//! reorders text around ([`crate::tty::is_active`]: DEL, C1, the bidi marks,
//! embeddings, overrides and isolates, U+2028/U+2029) is escaped as `\uXXXX`
//! too. That is still valid JSON and decodes to the same string, so no parser
//! sees a difference; only the raw bytes stop carrying instructions. Every
//! other character, non-ASCII included, is verbatim UTF-8, so a styled frame
//! of CJK text stays its size.
//!
//! `aterm_link::glance`'s `json_string` stays separate because it escapes every
//! character outside printable ASCII for an ASCII-only file contract.

/// `s` as a JSON string body (no quotes) — see the module docs.
#[must_use]
#[inline]
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len().saturating_add(2));
    escape_into(&mut out, s);
    out
}

/// [`escape`] appending into a caller-owned buffer, with no allocation of its
/// own. Hot: the styled frame escapes a glyph for every cell (up to ~15 000
/// per snapshot, per subscriber per change), so a run of plain bytes is copied
/// whole, and only a lead byte that can start an active character is decoded.
pub fn escape_into(out: &mut String, s: &str) {
    let bytes = s.as_bytes();
    // `start..at` is the plain run not yet copied; both are always on a
    // character boundary.
    let mut start = 0;
    let mut at = 0;
    while let Some(&b) = bytes.get(at) {
        if (0x20..0x7f).contains(&b) && b != b'"' && b != b'\\' {
            at += 1;
            continue;
        }
        if b >= 0x80 {
            // Every active character past ASCII starts with C2 (C1), D8
            // (U+061C) or E2 (U+200E-U+2069); any other lead starts text.
            let len = utf8_len(b);
            if matches!(b, 0xc2 | 0xd8 | 0xe2)
                && let Some(c) = s.get(at..).and_then(|rest| rest.chars().next())
                && crate::tty::is_active(c)
            {
                out.push_str(s.get(start..at).unwrap_or(""));
                push_u(out, c as u32);
                at += c.len_utf8();
                start = at;
                continue;
            }
            at += len;
            continue;
        }
        out.push_str(s.get(start..at).unwrap_or(""));
        match b {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            0x08 => out.push_str("\\b"),
            0x0c => out.push_str("\\f"),
            // The rest of C0, and DEL.
            b => push_u(out, u32::from(b)),
        }
        at += 1;
        start = at;
    }
    out.push_str(s.get(start..).unwrap_or(""));
}

/// The length of the UTF-8 character a valid lead byte `b` (>= 0x80) starts.
#[inline]
const fn utf8_len(b: u8) -> usize {
    if b >= 0xf0 {
        4
    } else if b >= 0xe0 {
        3
    } else {
        2
    }
}

/// `\uXXXX`, lowercase: every escaped scalar here is in the BMP.
#[inline]
fn push_u(out: &mut String, u: u32) {
    out.push_str("\\u");
    crate::hex::push_lower(out, &[(u >> 8) as u8, u as u8]);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The escape as RFC 8259 states it, plus the terminal-active class: the
    /// reference the fast path is held to.
    fn reference(s: &str) -> String {
        let mut out = String::new();
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                '\u{8}' => out.push_str("\\b"),
                '\u{c}' => out.push_str("\\f"),
                c if crate::tty::is_active(c) => out.push_str(&format!("\\u{:04x}", c as u32)),
                c => out.push(c),
            }
        }
        out
    }

    /// The wire's spelling is unchanged for what it escaped before, and the
    /// active class past C0 is now `\uXXXX` too.
    #[test]
    fn escape_spells_rfc_8259_and_the_active_class() {
        for (raw, json) in [
            ("plain", "plain"),
            ("a\"b\\c", "a\\\"b\\\\c"),
            ("tab\tnl\ncr\r", "tab\\tnl\\ncr\\r"),
            ("\u{8}\u{c}\u{1}\u{1b}[2K", "\\b\\f\\u0001\\u001b[2K"),
            ("café 日本 🙂", "café 日本 🙂"),
            ("\u{7f}", "\\u007f"),
            (
                "CWD\u{9b}31m\u{9d}0;t\u{9c}",
                "CWD\\u009b31m\\u009d0;t\\u009c",
            ),
            ("\u{202e}dcba\u{2066}\u{2069}", "\\u202edcba\\u2066\\u2069"),
            ("\u{61c}\u{200e}\u{200f}", "\\u061c\\u200e\\u200f"),
            ("a\u{2028}b\u{2029}", "a\\u2028b\\u2029"),
            // Leads the fast path decodes, starting text: NBSP, Arabic, ZWSP.
            (
                "\u{a0}\u{627}\u{200b}\u{2014}",
                "\u{a0}\u{627}\u{200b}\u{2014}",
            ),
            ("", ""),
        ] {
            assert_eq!(escape(raw), json, "{raw:?}");
        }
    }

    /// The fast path equals the reference over every one- and two-byte scalar
    /// and a stride of wider ones, alone and inside text.
    #[test]
    fn the_fast_path_is_the_reference() {
        let narrow = (0u32..0x800).filter_map(char::from_u32);
        let wider = (0x800u32..=0x10_ffff).step_by(7).filter_map(char::from_u32);
        let wide = (0x2000u32..0x2100).filter_map(char::from_u32);
        for c in narrow.chain(wider).chain(wide) {
            let s = format!("a{c}b\"{c}");
            assert_eq!(escape(&s), reference(&s), "{:04x}", c as u32);
        }
        let mut into = String::from("k=");
        escape_into(&mut into, "a\u{9b}");
        assert_eq!(into, "k=a\\u009b");
    }

    /// No byte a terminal acts on is left in the output, and a JSON reader
    /// decodes the output back to the input: the escape is the same string.
    #[test]
    fn the_output_holds_no_active_byte_and_decodes_back() {
        let raw =
            "/tmp/CWD\u{1b}]52;c;Q1dE\u{7}\u{1b}[2K\rFAKE\u{9b}31m\u{202e}dcba\u{2028}\u{7f}é";
        let json = escape(raw);
        assert!(!json.chars().any(crate::tty::is_active), "{json:?}");
        assert_eq!(decode(&json), raw);
    }

    /// A JSON string body's decoder (`\uXXXX` and the two-character escapes):
    /// enough of RFC 8259 to read back what [`escape`] writes.
    fn decode(json: &str) -> String {
        let mut out = String::new();
        let mut chars = json.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('b') => out.push('\u{8}'),
                Some('f') => out.push('\u{c}'),
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).collect();
                    let u = u32::from_str_radix(&hex, 16).expect("four hex digits");
                    out.push(char::from_u32(u).expect("a BMP scalar"));
                }
                Some(other) => out.push(other),
                None => panic!("a dangling backslash"),
            }
        }
        out
    }
}
