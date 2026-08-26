// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The response-framing primitives every verb shares: glyph sanitization,
//! percent-encoding for single-token fields, and the ONE JSON escape.
//!
//! SINGLE SOURCE OF TRUTH: these are re-exported by `aterm-gui`'s `control`
//! module under their existing names, so the poll face, the push face and the
//! asciicast emitter keep producing byte-identical bytes. A hand-maintained
//! duplicate has already drifted once; do not make a second copy.

/// Map a [`RenderCell`](aterm_core::terminal::RenderCell) char to its on-screen
/// glyph, collapsing NUL/control chars to a space. Shared so the push face
/// produces byte-identical rows to the poll face.
#[must_use]
pub fn visible_char(ch: char) -> char {
    if ch == '\0' || ch.is_control() {
        ' '
    } else {
        ch
    }
}

/// Percent-encode a string so it occupies ONE space-free token in a response
/// line: every byte that is not ASCII-graphic (and `%` itself) becomes `%XX`.
/// Spaces, newlines and non-ASCII are escaped; the client decodes. Empty -> "".
#[must_use]
pub fn pct_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    pct_encode_into(&mut out, s);
    out
}

/// [`pct_encode`] APPENDING into a caller-owned buffer — byte-identical output,
/// no allocation of its own (the twin of [`json_escape_into`], for the same
/// reason: `blocks` and `history` encode up to a thousand records per reply,
/// under the terminal lock, and every one of them was paying a `String` per
/// field purely to be copied into the response buffer and dropped).
pub fn pct_encode_into(out: &mut String, s: &str) {
    // Nibble table rather than `format!("%{b:02X}")`: the `format!` arm
    // allocated a throwaway `String` AND ran the whole `core::fmt` machinery for
    // every escaped byte — and EVERY byte takes that arm for non-ASCII text.
    // `{:02X}` on a `u8` is always exactly two zero-padded UPPERCASE hex digits,
    // which is precisely what this table emits, so the bytes are unchanged.
    // (Uppercase on purpose: `aterm_uds::rand::hex_encode`'s table is lowercase
    // and must not be copied here.)
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

/// Decode [`pct_encode`]'s output: every `%XX` hex pair becomes its byte;
/// malformed escapes pass through verbatim and invalid UTF-8 decodes lossily,
/// so this is TOTAL — safe on bytes a hostile peer authored. The decoder
/// lives beside the encoder so the pair cannot drift.
#[must_use]
pub fn pct_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Escape a string as a JSON string BODY (no surrounding quotes): the two-char
/// escapes for `"`, `\`, and the C0 whitespace controls, and `\u00XX` for the
/// remaining control bytes. Non-ASCII UTF-8 is emitted verbatim (a JSON string is
/// UTF-8), so this is allocation-light for ordinary text. Shared by every `*_json`
/// emitter so the `--json` read mode produces RFC 8259-valid strings, and by the
/// asciicast emitter, so there is no second escape to diverge from.
#[must_use]
pub fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    json_escape_into(&mut out, s);
    out
}

/// [`json_escape`] APPENDING into a caller-owned buffer — byte-identical output,
/// no allocation of its own.
///
/// The lossless styled frame escapes a glyph (and sometimes a hyperlink) for
/// EVERY cell on screen — up to ~15 000 per snapshot on a large window, and
/// again for every subscriber on every change — so the allocating twin above was
/// paying a `String` per cell purely to be copied into the row buffer and
/// dropped. This is the same loop writing where the answer is already going.
pub fn json_escape_into(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                use std::fmt::Write as _;
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
}

/// A `"key":"<escaped>"` JSON member.
#[must_use]
pub fn json_str_field(key: &str, val: &str) -> String {
    format!("\"{key}\":\"{}\"", json_escape(val))
}

/// Wrap a one-line JSON object body in the read-verb framing: `OK 1\n<json>\n`.
/// The framing matches the other read verbs (`OK <n>` header + body) so the
/// EXISTING client streams the body identically whether or not `--json` is set —
/// only the body bytes change. A JSON reply is always a single body line.
#[must_use]
pub fn json_ok(body: &str) -> String {
    format!("OK 1\n{body}\n")
}

#[cfg(test)]
mod pct_tests {
    use super::{pct_decode, pct_encode};

    /// The decoder is total and the encoder's exact inverse on real strings;
    /// malformed escapes pass through; invalid UTF-8 decodes lossily.
    #[test]
    fn pct_decode_is_total_and_round_trips() {
        for s in [
            "",
            "plain",
            "with space",
            "构建 agent ✨",
            "100%",
            "a-b_c",
            "-",
        ] {
            assert_eq!(pct_decode(&pct_encode(s)), s, "round-trip: {s:?}");
        }
        assert_eq!(pct_decode("%G1"), "%G1", "malformed hex passes through");
        assert_eq!(pct_decode("%2"), "%2", "truncated escape passes through");
        assert_eq!(pct_decode("trail%"), "trail%");
        // An escape decoding to invalid UTF-8 is replaced, never a panic.
        assert_eq!(pct_decode("%FF"), "\u{FFFD}");
    }
}
