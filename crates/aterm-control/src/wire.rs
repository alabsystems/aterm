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
    for b in s.bytes() {
        if b.is_ascii_graphic() && b != b'%' {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
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
