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

/// The bytes [`encode`] makes of `s`: one for an ASCII-graphic byte other than `%`, three for
/// every other byte.
#[must_use]
#[inline]
pub fn encoded_len(s: &str) -> usize {
    s.bytes()
        .map(|b| {
            if b.is_ascii_graphic() && b != b'%' {
                1
            } else {
                3
            }
        })
        .sum()
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
#[path = "pct_tests.rs"]
mod tests;
