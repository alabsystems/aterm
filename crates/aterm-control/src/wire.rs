// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The response-framing primitives every verb shares: glyph sanitization and the one collapse-and-trim rule for a row of text ([`push_visible`]) with its blank-tail count ([`trimmed_len`]), percent-encoding for single-token fields, the one JSON escape and the typed record sinks ([`fields`]) (these three re-exported from `aterm_codec`), and the one reading of a wait verb's `<ms>` ([`wait_ms`]).

/// Map a [`RenderCell`](aterm_core::terminal::RenderCell) char to its on-screen glyph, collapsing NUL/control chars to a space.
#[must_use]
#[inline]
pub const fn visible_char(ch: char) -> char {
    // NUL and the rest of `char::is_control` (general category Cc), spelled
    // out: `is_control` is not a `const fn` on stable Rust.
    if matches!(ch, '\u{0}'..='\u{1f}' | '\u{7f}'..='\u{9f}') {
        ' '
    } else {
        ch
    }
}

/// What [`push_visible`] drops from the end of the text it appends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VisibleTrim {
    /// Nothing: `offscreen`'s archived rows, stored already shown and trimmed.
    None,
    /// Trailing U+0020 only: `line`'s rule.
    Spaces,
    /// Trailing Unicode White_Space ([`str::trim_end`]): a screen row as `text`, the `status` hash and `cast drift` read it ([`visible_row`]), and each row of `blocktext`.
    Whitespace,
}

/// Append `s` to `out` as the wire shows text: every NUL and control char a space ([`visible_char`]), then the appended text's tail trimmed per `trim`, so a row ending in a control char ends no differently from one ending in a blank.
pub fn push_visible(out: &mut String, s: &str, trim: VisibleTrim) {
    let start = out.len();
    out.push_str(s);
    show_tail(out, start, trim);
}

/// [`push_visible`]'s rule applied to the text already at `out[start..]`.
fn show_tail(out: &mut String, start: usize, trim: VisibleTrim) {
    if out[start..].chars().any(|c| visible_char(c) != c) {
        let raw = out.split_off(start);
        out.extend(raw.chars().map(visible_char));
    }
    let kept = match trim {
        VisibleTrim::None => out.len(),
        VisibleTrim::Spaces => start + out[start..].trim_end_matches(' ').len(),
        VisibleTrim::Whitespace => start + out[start..].trim_end().len(),
    };
    out.truncate(kept);
}

/// Whether `row` shows nothing: every char is whitespace once collapsed ([`visible_char`]), so [`push_visible`] under [`VisibleTrim::Whitespace`] appends nothing for it.
#[must_use]
#[inline]
pub fn is_blank_row(row: &str) -> bool {
    row.chars().all(|c| visible_char(c).is_whitespace())
}

/// How many leading rows a reply keeps once its trailing blank rows ([`is_blank_row`]) are dropped: the index of the last row that is not blank plus one, and 0 when every row is blank.
pub fn trimmed_len<'a>(rows: impl Iterator<Item = &'a str>) -> usize {
    rows.enumerate()
        .filter(|(_, r)| !is_blank_row(r))
        .map(|(i, _)| i + 1)
        .last()
        .unwrap_or(0)
}

/// The visible, trailing-trimmed text of screen row `r`: the engine's combining-aware `get_line_text` with interior control chars collapsed to spaces and the tail trimmed ([`push_visible`] under [`VisibleTrim::Whitespace`]).
#[must_use]
pub fn visible_row(t: &aterm_core::terminal::Terminal, r: usize) -> String {
    let mut out = String::with_capacity(usize::from(t.cols()));
    visible_row_into(t, r, &mut out);
    out
}

/// [`visible_row`] appended to `out`, with no `String` of its own: the engine appends the row ([`Terminal::push_line_text`](aterm_core::terminal::Terminal::push_line_text)) and [`push_visible`]'s rule collapses and trims it where it landed.
pub fn visible_row_into(t: &aterm_core::terminal::Terminal, r: usize, out: &mut String) {
    let start = out.len();
    t.push_line_text(r, out);
    show_tail(out, start, VisibleTrim::Whitespace);
}

/// The word a reply carries when the program's DEC 2026 frame was still unfinished at the read's bound.
pub const FRAME_OPEN_WORD: &str = "frame=open";

/// Whether `t`'s DEC 2026 frame is unfinished now, reading the clock only while one is open.
#[must_use]
#[inline]
pub fn frame_unfinished_now(t: &aterm_core::terminal::Terminal) -> bool {
    t.sync_frame_unfinished() && t.frame_unfinished_at(std::time::Instant::now())
}

/// ` frame=open` when `open`, else nothing: a header's trailing word.
#[must_use]
#[inline]
pub const fn frame_open_tail(open: bool) -> &'static str {
    if open { " frame=open" } else { "" }
}

/// The control wire's percent escape under the names verbs and hosts use: [`pct_encode`] makes one space-free token (non-graphic bytes and `%` as uppercase `%XX`), [`pct_encode_into`] appends into a caller buffer, and [`pct_decode`] is the total inverse.
pub use aterm_codec::pct::{
    decode as pct_decode, encode as pct_encode, encode_into as pct_encode_into,
};

/// The wait family's default timeout (`wait`, `ready`, `await`): 30 s.
pub const WAIT_DEFAULT_MS: u64 = 30_000;

/// The longest any wait verb waits: 600 s, whatever `<ms>` asked.
pub const WAIT_MAX_MS: u64 = 600_000;

/// Clamp a wait duration in milliseconds to `[0, WAIT_MAX_MS]`.
#[must_use]
#[inline]
pub const fn clamp_wait_ms(ms: u64) -> u64 {
    if ms > WAIT_MAX_MS { WAIT_MAX_MS } else { ms }
}

/// Resolve an optional `<ms>` wait parameter to [`WAIT_DEFAULT_MS`] when `None` or `clamp_wait_ms(ms)` when `Some(ms)`.
#[must_use]
#[inline]
pub const fn resolve_wait_timeout_ms(requested: Option<u64>) -> u64 {
    match requested {
        Some(ms) => clamp_wait_ms(ms),
        None => WAIT_DEFAULT_MS,
    }
}

/// A wait verb's `<ms>`: a whole number of milliseconds (ASCII digits only), held at [`WAIT_MAX_MS`].
#[must_use]
#[inline]
pub fn wait_ms(token: &str) -> Option<u64> {
    if token.is_empty() || !token.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    token.parse::<u64>().ok().map(clamp_wait_ms)
}

/// The one JSON string escape under the names every `*_json` emitter, the `subscribe … cells` frame and the asciicast emitter use: [`json_escape`] returns a string body (no quotes), [`json_escape_into`] appends into a caller buffer (the styled frame escapes a glyph per cell).
pub use aterm_codec::json::{escape as json_escape, escape_into as json_escape_into};

/// The typed record sinks: a producer writes each field once to a [`fields::FieldSink`], and [`fields::TextSink`] spells the `key=value` line while [`fields::JsonSink`] spells the `--json` object, so a verb's two forms come from one emit function.
pub use aterm_codec::fields;

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

/// Wrap a one-line JSON object body in the read-verb framing: `OK 1\n<json>\n`, the same `OK <n>` framing as the other read verbs, so clients stream it unchanged.
#[must_use]
pub fn json_ok(body: &str) -> String {
    format!("OK 1\n{body}\n")
}

#[cfg(test)]
#[path = "wire_tests.rs"]
mod tests;
