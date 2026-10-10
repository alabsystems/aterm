// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The response-framing primitives every verb shares: glyph sanitization,
//! percent-encoding for single-token fields, and the one JSON escape; and the
//! one reading of a wait verb's `<ms>` ([`wait_ms`]).
//! `aterm-gui`'s `control` module re-exports them, so the poll face, the push
//! face and the asciicast emitter produce identical bytes; keep one copy.

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

/// The visible, trailing-trimmed text of screen row `r`: the engine's
/// combining-aware `get_line_text` with interior control chars collapsed to
/// spaces ([`visible_char`]) and the tail trimmed. The single source for a
/// row's text: `aterm-gui`'s `visible_row` (behind `text`, `text --json`, the
/// `subscribe screen` delta and the `status` hash) delegates here, as does
/// `cast drift`. Caller holds whatever lock guards `t`.
#[must_use]
pub fn visible_row(t: &aterm_core::terminal::Terminal, r: usize) -> String {
    let line = t.get_line_text(r as i32, None).unwrap_or_default();
    let mut out: String = line.chars().map(visible_char).collect();
    // Truncate in place (a prefix of the same bytes) rather than allocate a
    // trimmed copy: this runs per row per `subscribe screen` push with the
    // terminal lock held. Two statements, since
    // `out.truncate(out.trim_end().len())` cannot borrow-check.
    let end = out.trim_end().len();
    out.truncate(end);
    out
}

/// The control wire's percent escape under the names verbs and hosts use:
/// [`pct_encode`] makes one space-free token (non-graphic bytes and `%` as
/// uppercase `%XX`), [`pct_encode_into`] appends into a caller buffer, and
/// [`pct_decode`] is the total inverse. The implementation lives in
/// dependency-free `aterm_codec::pct`, shared with clients and wasm32 hosts.
pub use aterm_codec::pct::{
    decode as pct_decode, encode as pct_encode, encode_into as pct_encode_into,
};

/// The wait family's default timeout (`wait`, `ready`, `await`): 30 s.
pub const WAIT_DEFAULT_MS: u64 = 30_000;

/// The longest any wait verb waits: 600 s, whatever `<ms>` asked.
pub const WAIT_MAX_MS: u64 = 600_000;

/// A wait verb's `<ms>`: a whole number of milliseconds, held at
/// [`WAIT_MAX_MS`]. `None` for anything else (`2s`, `1.5`, `-1`, empty), which
/// the verb answers with its usage line: read leniently, `wait 2s` waited the
/// whole 30 s default where `turn timeout=2s` and `await consent timeout=2s`
/// refused it. One reading, so the family cannot disagree again.
#[must_use]
pub fn wait_ms(token: &str) -> Option<u64> {
    token.parse::<u64>().ok().map(|ms| ms.min(WAIT_MAX_MS))
}

/// Escape a string as a JSON string body (no quotes): two-char escapes for
/// `"`, `\` and the C0 whitespace controls, `\u00XX` for other control bytes;
/// non-ASCII UTF-8 verbatim. Shared by every `*_json` emitter and the
/// asciicast emitter.
#[must_use]
pub fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    json_escape_into(&mut out, s);
    out
}

/// [`json_escape`] appending into a caller-owned buffer, with no allocation of
/// its own. The styled frame escapes a glyph for every cell (up to ~15 000 per
/// snapshot, per subscriber per change).
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

/// Wrap a one-line JSON object body in the read-verb framing:
/// `OK 1\n<json>\n`, the same `OK <n>` framing as the other read verbs, so
/// clients stream it unchanged.
#[must_use]
pub fn json_ok(body: &str) -> String {
    format!("OK 1\n{body}\n")
}
