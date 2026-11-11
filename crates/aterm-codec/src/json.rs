// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! JSON string escaping for control-path JSON writers.

/// Escapes `s` as a JSON string body (without surrounding quotes).
#[must_use]
#[inline]
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len().saturating_add(2));
    escape_into(&mut out, s);
    out
}

/// Appends `s` escaped as a JSON string body into `out`.
pub fn escape_into(out: &mut String, s: &str) {
    let bytes = s.as_bytes();
    let mut start = 0;
    let mut at = 0;
    while let Some(&b) = bytes.get(at) {
        if (0x20..0x7f).contains(&b) && b != b'"' && b != b'\\' {
            at += 1;
            continue;
        }
        if b >= 0x80 {
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
#[path = "json_tests.rs"]
mod tests;
