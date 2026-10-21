// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The response-framing primitives every verb shares: glyph sanitization and
//! the one collapse-and-trim rule for a row of text ([`push_visible`]) with its
//! blank-tail count ([`trimmed_len`]), percent-encoding for single-token fields,
//! the one JSON escape and the typed record sinks ([`fields`]) (these three
//! re-exported from `aterm_codec`), and the one reading of a wait verb's `<ms>`
//! ([`wait_ms`]).
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

/// What [`push_visible`] drops from the end of the text it appends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VisibleTrim {
    /// Nothing: `offscreen`'s archived rows, stored already shown and trimmed.
    None,
    /// Trailing U+0020 only: `line`'s rule. Its row is the line as written,
    /// blanks the program wrote included, so the spaces padding it go while a
    /// written NBSP or U+3000 stays text.
    Spaces,
    /// Trailing Unicode White_Space ([`str::trim_end`]): a screen row as `text`,
    /// the `status` hash and `cast drift` read it ([`visible_row`]), and each row
    /// of `blocktext`.
    Whitespace,
}

/// Append `s` to `out` as the wire shows text: every NUL and control char a
/// space ([`visible_char`]), then the appended text's tail trimmed per `trim`,
/// so a row ending in a control char ends no differently from one ending in a
/// blank. What `out` held is never changed. Text with no control char (nearly
/// every row) costs one `push_str` and a scan. The collapse-and-trim rule:
/// [`visible_row`], `line`, `offscreen` and `blocktext` all append through it.
pub fn push_visible(out: &mut String, s: &str, trim: VisibleTrim) {
    let start = out.len();
    out.push_str(s);
    show_tail(out, start, trim);
}

/// [`push_visible`]'s rule applied to the text already at `out[start..]`.
fn show_tail(out: &mut String, start: usize, trim: VisibleTrim) {
    if out[start..].chars().any(|c| visible_char(c) != c) {
        // Rare (a control char a cell or a stored line kept): the collapse can
        // shorten the text (a C1 char is two bytes, its space one), so the tail
        // is rebuilt rather than patched.
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

/// Whether `row` shows nothing: every char is whitespace once collapsed
/// ([`visible_char`]), so [`push_visible`] under [`VisibleTrim::Whitespace`]
/// appends nothing for it. A raw line and its shown form answer alike, so a
/// reply can count its blank tail before it frames a row.
#[must_use]
pub fn is_blank_row(row: &str) -> bool {
    row.chars().all(|c| visible_char(c).is_whitespace())
}

/// How many leading rows a reply keeps once its trailing blank rows
/// ([`is_blank_row`]) are dropped: the index of the last row that is not blank
/// plus one, and 0 when every row is blank. Interior blank rows are kept, only
/// the tail goes, so row `i` of a trimmed reply is still row `i` of the read,
/// and the `line`/`cell`/`search` coordinates an agent reads off it stay valid.
/// The blank-tail rule: `text trim`, `text --json trim`, `turn … trim=1`,
/// `blocktext … trim`, `temporal … trim` and the `subscribe … screen,trim` delta
/// all count with it, so the polled and pushed faces cannot disagree about what
/// a trailing blank is. The count on a trimmed header is the count of rows
/// actually sent.
pub fn trimmed_len<'a>(rows: impl Iterator<Item = &'a str>) -> usize {
    rows.enumerate()
        .filter(|(_, r)| !is_blank_row(r))
        .map(|(i, _)| i + 1)
        .last()
        .unwrap_or(0)
}

/// The visible, trailing-trimmed text of screen row `r`: the engine's
/// combining-aware `get_line_text` with interior control chars collapsed to
/// spaces and the tail trimmed ([`push_visible`] under
/// [`VisibleTrim::Whitespace`]). The single source for a row's text:
/// `aterm-gui`'s `visible_row` (behind `text`, `text --json`, the `subscribe
/// screen` delta and the `status` hash) delegates here, as does `cast drift`.
/// Caller holds whatever lock guards `t`.
#[must_use]
pub fn visible_row(t: &aterm_core::terminal::Terminal, r: usize) -> String {
    let mut out = String::with_capacity(usize::from(t.cols()));
    visible_row_into(t, r, &mut out);
    out
}

/// [`visible_row`] appended to `out`, with no `String` of its own: the engine
/// appends the row ([`Terminal::push_line_text`]) and [`push_visible`]'s rule
/// collapses and trims it where it landed. What `out` held is kept; a row off
/// the screen appends nothing. The whole-screen text (`aterm-gui`'s
/// `ScreenText`, built once per frame change) writes every row straight into
/// its one buffer through this. Caller holds whatever lock guards `t`.
///
/// [`Terminal::push_line_text`]: aterm_core::terminal::Terminal::push_line_text
pub fn visible_row_into(t: &aterm_core::terminal::Terminal, r: usize, out: &mut String) {
    let start = out.len();
    t.push_line_text(r, out);
    show_tail(out, start, VisibleTrim::Whitespace);
}

/// The control wire's percent escape under the names verbs and hosts use:
/// [`pct_encode`] makes one space-free token (non-graphic bytes and `%` as
/// uppercase `%XX`), [`pct_encode_into`] appends into a caller buffer, and
/// [`pct_decode`] is the total inverse. The implementation lives in
/// `aterm_codec::pct` (std plus the dependency-free astream-wire, no platform
/// code), shared with clients and wasm32 hosts.
pub use aterm_codec::pct::{
    decode as pct_decode, encode as pct_encode, encode_into as pct_encode_into,
};

/// The wait family's default timeout (`wait`, `ready`, `await`): 30 s.
pub const WAIT_DEFAULT_MS: u64 = 30_000;

/// The longest any wait verb waits: 600 s, whatever `<ms>` asked.
pub const WAIT_MAX_MS: u64 = 600_000;

/// Clamp a wait duration in milliseconds to `[0, WAIT_MAX_MS]`.
#[must_use]
pub const fn clamp_wait_ms(ms: u64) -> u64 {
    if ms > WAIT_MAX_MS { WAIT_MAX_MS } else { ms }
}

/// Resolve an optional `<ms>` wait parameter to [`WAIT_DEFAULT_MS`] when `None`
/// or `clamp_wait_ms(ms)` when `Some(ms)`.
#[must_use]
pub const fn resolve_wait_timeout_ms(requested: Option<u64>) -> u64 {
    match requested {
        Some(ms) => clamp_wait_ms(ms),
        None => WAIT_DEFAULT_MS,
    }
}

/// Return `true` iff `ms <= WAIT_MAX_MS`.
#[must_use]
pub const fn wait_within_bounds(ms: u64) -> bool {
    ms <= WAIT_MAX_MS
}

/// Bound one park slice of a waiting verb by `poll_ms` (flooring `poll_ms` at `1`).
#[must_use]
pub const fn hangup_slice_ms(remaining_ms: u64, poll_ms: u64) -> u64 {
    let p = if poll_ms == 0 { 1 } else { poll_ms };
    if remaining_ms < p { remaining_ms } else { p }
}

/// A wait verb's `<ms>`: a whole number of milliseconds, held at
/// [`WAIT_MAX_MS`]. `None` for anything else (`2s`, `1.5`, `-1`, empty), which
/// the verb answers with its usage line: read leniently, `wait 2s` waited the
/// whole 30 s default where `turn timeout=2s` and `await consent timeout=2s`
/// refused it. One reading, so the family cannot disagree again.
#[must_use]
pub fn wait_ms(token: &str) -> Option<u64> {
    token.parse::<u64>().ok().map(clamp_wait_ms)
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
/// implementation is `aterm_codec::json`, shared with the supervisor's JSON.
pub use aterm_codec::json::{escape as json_escape, escape_into as json_escape_into};

/// The typed record sinks: a producer writes each field once to a
/// [`fields::FieldSink`], and [`fields::TextSink`] spells the `key=value` line
/// while [`fields::JsonSink`] spells the `--json` object, so a verb's two forms
/// come from one emit function. Shared with the supervisor, which reaches
/// `aterm_codec` but not this crate.
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

    #[test]
    fn wait_and_hangup_const_helpers_invariants() {
        assert_eq!(clamp_wait_ms(1_000), 1_000);
        assert_eq!(clamp_wait_ms(u64::MAX), WAIT_MAX_MS);
        assert_eq!(resolve_wait_timeout_ms(None), WAIT_DEFAULT_MS);
        assert_eq!(resolve_wait_timeout_ms(Some(900_000)), WAIT_MAX_MS);
        assert!(wait_within_bounds(WAIT_MAX_MS));
        assert!(!wait_within_bounds(WAIT_MAX_MS + 1));
        assert_eq!(hangup_slice_ms(50, 200), 50);
        assert_eq!(hangup_slice_ms(500, 200), 200);
        assert_eq!(hangup_slice_ms(500, 0), 1);
    }

    /// `push_visible` collapses first and trims second, under each rule, and
    /// never touches what the buffer held. Negative control: trimming before the
    /// collapse (the order `text order=display` reads its rows in) leaves a space
    /// where a row ends in a control char.
    #[test]
    fn push_visible_collapses_then_trims_each_rule() {
        use VisibleTrim::{None, Spaces, Whitespace};
        let cases: &[(&str, VisibleTrim, &str)] = &[
            ("a\x01b\0c", None, "a b c"),
            ("a\u{9b}b", None, "a b"),
            ("plain  ", None, "plain  "),
            ("x\u{a0}  ", Spaces, "x\u{a0}"),
            ("x\u{3000}\x01", Spaces, "x\u{3000}"),
            ("x\x01", Spaces, "x"),
            ("x\u{a0}", Whitespace, "x"),
            ("x\u{3000}", Whitespace, "x"),
            ("x\u{2028}", Whitespace, "x"),
            ("   \0\t", Whitespace, ""),
            ("x\u{3000}\x01", Whitespace, "x"),
            ("  lead", Whitespace, "  lead"),
        ];
        for &(raw, trim, want) in cases {
            let mut out = String::from("keep  ");
            push_visible(&mut out, raw, trim);
            assert_eq!(out, format!("keep  {want}"), "{raw:?} under {trim:?}");
        }
        let mut out = String::from("keep  ");
        push_visible(&mut out, " \x01 ", Whitespace);
        assert_eq!(
            out, "keep  ",
            "a blank row appends nothing and trims nothing before it"
        );

        let raw = "x\u{3000}\x01";
        let trimmed_first: String = raw.trim_end().chars().map(visible_char).collect();
        assert_eq!(
            trimmed_first, "x\u{3000} ",
            "control: the other order keeps the tail"
        );
    }

    /// The blank rule counts a raw line and its shown form alike, which is what
    /// lets `blocktext` count its tail before it frames a row; interior blank rows
    /// stay. Negative control: a spaces-only blank test counts an NBSP or a
    /// control char as content.
    #[test]
    fn trimmed_len_is_last_nonblank_plus_one() {
        assert_eq!(trimmed_len(["", "", ""].into_iter()), 0, "all blank");
        assert_eq!(trimmed_len(["   ", " "].into_iter()), 0, "spaces are blank");
        assert_eq!(trimmed_len(std::iter::empty()), 0, "no rows");
        assert_eq!(trimmed_len(["a", "b", "c"].into_iter()), 3, "no blank → n");
        assert_eq!(
            trimmed_len(["a", "", "", "b", "", ""].into_iter()),
            4,
            "interior blanks kept, only the tail dropped"
        );
        assert_eq!(trimmed_len(["", "x"].into_iter()), 2, "leading blank kept");

        let raw = ["out", "\u{a0}", "\x01 \t", "\0"];
        let shown: Vec<String> = raw
            .iter()
            .map(|r| {
                let mut s = String::new();
                push_visible(&mut s, r, VisibleTrim::Whitespace);
                s
            })
            .collect();
        assert_eq!(trimmed_len(raw.into_iter()), 1);
        assert_eq!(trimmed_len(shown.iter().map(String::as_str)), 1);
        let spaces_only = raw
            .iter()
            .rposition(|r| !r.trim_matches(' ').is_empty())
            .map_or(0, |i| i + 1);
        assert_eq!(spaces_only, 4, "control: the old rule keeps every row");
    }

    /// `visible_row` matches a two-pass reading (the engine's line, collapsed,
    /// then trimmed) byte for byte on every row, scrolled back or not, and
    /// `visible_row_into` appends exactly it after what the buffer held.
    #[test]
    fn visible_row_into_appends_the_rows_text_reads() {
        let mut term = aterm_core::terminal::Terminal::new(5, 16);
        for i in 0..12 {
            term.process(format!("line {i}\r\n").as_bytes());
        }
        term.process("e\u{301} 界 nb\u{a0}  \r\nwide 界界".as_bytes());
        term.scroll_display(3);
        for r in 0..7 {
            let line = i32::try_from(r)
                .ok()
                .and_then(|row| term.get_line_text(row, Option::None))
                .unwrap_or_default();
            let two_pass: String = line.chars().map(visible_char).collect();
            let want = two_pass.trim_end();
            assert_eq!(visible_row(&term, r), want, "row {r}");
            let mut out = String::from("pre\u{a0} ");
            visible_row_into(&term, r, &mut out);
            assert_eq!(out, format!("pre\u{a0} {want}"), "row {r}");
        }
        assert_eq!(
            visible_row(&term, 4),
            "wide 界界",
            "precondition: the fixture is on screen"
        );
    }
}
