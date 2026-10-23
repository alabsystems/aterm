// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The control wire's percent escape, shared by every speaker of the wire.

/// Percent-encode `s` into one space-free token: every byte that is not ASCII-graphic, and `%`, becomes `%XX`.
#[must_use]
#[inline]
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    encode_into(&mut out, s);
    out
}

/// [`encode`] appending into a caller-owned buffer, with no allocation of its own.
#[inline]
pub fn encode_into(out: &mut String, s: &str) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for b in s.bytes() {
        if b.is_ascii_graphic() && b != b'%' {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX[usize::from(b >> 4)] as char);
            out.push(HEX[usize::from(b & 0x0f)] as char);
        }
    }
}

/// [`encode`]'s narrow sibling for one word of an argv carried as a line of space-separated words ([`join_words`]).
#[must_use]
pub fn encode_word(s: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == '%' || c.is_whitespace() || c.is_control() {
            let mut buf = [0u8; 4];
            for b in c.encode_utf8(&mut buf).bytes() {
                out.push('%');
                out.push(HEX[usize::from(b >> 4)] as char);
                out.push(HEX[usize::from(b & 0x0f)] as char);
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// An argv as ONE LINE of words: each [`encode_word`]-ed, joined by one space.
#[must_use]
pub fn join_words<S: AsRef<str>>(words: &[S]) -> String {
    words
        .iter()
        .map(|w| encode_word(w.as_ref()))
        .collect::<Vec<_>>()
        .join(" ")
}

/// [`join_words`]'s inverse: the line split on whitespace, each word [`decode`]d.
#[must_use]
#[inline]
pub fn split_words(line: &str) -> Vec<String> {
    line.split_whitespace().map(decode).collect()
}

/// Decode [`encode`]'s output: `%XX` (either case) becomes its byte; anything else passes through and invalid UTF-8 decodes lossily.
#[must_use]
pub fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((((hi & 0x0f) << 4) | (lo & 0x0f)) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt_spelling(s: &str) -> String {
        let mut out = String::new();
        for b in s.bytes() {
            if b.is_ascii_graphic() && b != b'%' {
                out.push(char::from(b));
            } else {
                out.push_str(&format!("%{b:02X}"));
            }
        }
        out
    }

    #[test]
    fn encode_is_the_wires_spelling_and_decode_inverts_it() {
        for (plain, encoded) in [
            ("hello", "hello"),
            ("two words", "two%20words"),
            ("a\nb", "a%0Ab"),
            ("100%", "100%25"),
            ("tab\there", "tab%09here"),
            ("é", "%C3%A9"),
            ("", ""),
        ] {
            assert_eq!(encode(plain), encoded, "encode {plain:?}");
            assert_eq!(decode(encoded), plain, "decode {encoded:?}");
        }
        let narrow = (0u32..0x800).filter_map(char::from_u32);
        let wider = (0x800u32..=0x10_ffff)
            .step_by(61)
            .filter_map(char::from_u32);
        for c in narrow.chain(wider) {
            let s = c.to_string();
            assert_eq!(encode(&s), fmt_spelling(&s), "{c:?}");
            assert_eq!(decode(&encode(&s)), s, "{c:?}");
        }
        for s in [
            "构建 agent ✨",
            "Details ›",
            "\t\n\r\u{7f}",
            "~/src/aterm",
            "-",
        ] {
            assert_eq!(decode(&encode(s)), s, "round-trip: {s:?}");
        }
        let mut into = String::from("k=");
        encode_into(&mut into, "a b");
        assert_eq!(into, "k=a%20b");
    }

    #[test]
    fn an_argv_line_keeps_every_word_whole() {
        let argv = [
            "/Users//u/Applications/aterm (dev).app/Contents/MacOS/aterm",
            "link",
            "100%",
            "tab\there",
            "line\nend",
            "nb\u{a0}sp",
            "Caf\u{e9}",
        ];
        let line = join_words(&argv);
        assert!(!line.contains('\n') && !line.contains('\t'), "{line}");
        assert_eq!(split_words(&line), argv, "{line}");
        assert_eq!(line.split_whitespace().count(), argv.len(), "{line}");
        let joined = argv.join(" ");
        let naive: Vec<&str> = joined.split_whitespace().collect();
        assert_ne!(naive, argv);
        let plain = [
            "/usr/bin/aterm",
            "link",
            "serve",
            "--state",
            "/Users//u/Caf\u{e9}",
        ];
        assert_eq!(join_words(&plain), plain.join(" "));
        assert_eq!(split_words(&plain.join(" ")), plain);
        assert_eq!(encode_word("a%b"), "a%25b");
        assert_eq!(encode_word("a b"), "a%20b");
        assert_eq!(encode_word("nb\u{a0}sp"), "nb%C2%A0sp");
    }

    #[test]
    fn decode_is_total() {
        for s in ["%G1", "%zz", "%2", "%4", "trail%", "%4G", "%é", "%+5"] {
            assert_eq!(decode(s), s, "passes through: {s:?}");
        }
        assert_eq!(decode("%%41"), "%A");
        assert_eq!(decode("%c3%a9"), "é", "lowercase hex decodes too");
        assert_eq!(decode("%FF"), "\u{FFFD}");
    }
}
