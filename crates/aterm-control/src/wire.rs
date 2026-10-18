// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The response-framing primitives every verb shares: glyph sanitization,
//! percent-encoding for single-token fields and the one JSON escape (these two
//! re-exported from `aterm_codec`), and the one reading of a wait verb's
//! `<ms>` ([`wait_ms`]).
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

/// The one JSON string escape under the names every `*_json` emitter, the
/// `subscribe … cells` frame and the asciicast emitter use: [`json_escape`]
/// returns a string body (no quotes), [`json_escape_into`] appends into a
/// caller buffer (the styled frame escapes a glyph per cell). Two-character
/// escapes for `"`, `\` and the C0 whitespace controls, `\u00XX` for the rest
/// of C0, and — because a reply is often read raw on a terminal — `\uXXXX`
/// for every character a terminal acts on or reorders text around (DEL, C1,
/// the bidi controls, U+2028/U+2029: a cwd from OSC 7 or a command line from
/// OSC 633 is text a program chose). Still valid JSON that decodes to the same
/// string; every other character, non-ASCII included, is verbatim UTF-8. The
/// implementation is `aterm_codec::json`, shared with the supervisor's JSON
/// (ruling 2026-10-10, round fifteen, OUT-2).
pub use aterm_codec::json::{escape as json_escape, escape_into as json_escape_into};

/// A JSON string member, escaping both its key and value.
#[must_use]
pub fn json_str_field(key: &str, val: &str) -> String {
    let mut out = String::from("\"");
    json_escape_into(&mut out, key);
    out.push_str("\":\"");
    json_escape_into(&mut out, val);
    out.push('"');
    out
}

/// Wrap a one-line JSON object body in the read-verb framing:
/// `OK 1\n<json>\n`, the same `OK <n>` framing as the other read verbs, so
/// clients stream it unchanged.
#[must_use]
pub fn json_ok(body: &str) -> String {
    format!("OK 1\n{body}\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_member_keeps_arbitrary_keys_and_values_in_one_member() {
        let key = "cwd\",\"injected\":\"\n\u{202e}";
        let raw = "quoted \"value\"\\\n界";
        let reply = format!("{{{}}}", json_str_field(key, raw));
        let value: aterm_json::Value = aterm_json::from_str(&reply).expect("valid JSON");
        assert_eq!(value.as_object().expect("object").len(), 1);
        assert_eq!(value[key].as_str(), Some(raw));
        assert!(!reply.contains(['\n', '\u{202e}']));
    }
}
