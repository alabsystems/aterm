// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Zero-dependency parser for `text --json`, `cell`, and `status` control replies.

use aterm_types::control_verbs::ScreenGen;
use std::time::Duration;

/// Parsed `text --json` screen snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Screen {
    /// Visible rows, top to bottom (or the requested `tail=` slice).
    pub rows: Vec<String>,
    /// 0-based cursor row in the full grid.
    pub cursor_row: usize,
    /// 0-based cursor column.
    pub cursor_col: usize,
    /// Content sequence number at the time of the read.
    pub seq: u64,
    /// Grid row index of `rows[0]` (`0` for a full read).
    pub first: usize,
    /// Screen generation token (`"<epoch>.<seq>"`) for fenced input.
    pub generation: Option<String>,
    /// Server-reported timestamp of the last human input.
    pub human: HumanInput,
    /// Monotonic count of human input gestures (`if-human=` fence value).
    pub human_seq: Option<u64>,
    /// Pane width in cells (`"dims.cols"`).
    pub cols: Option<u16>,
    /// Pane height in rows (`"dims.rows"`).
    pub pane_rows: Option<usize>,
    /// Person-initiated launch witness (`"launch"`), if active.
    pub launch: Option<LaunchWitness>,
    /// Total submissions written to the session's program (`"submits"`).
    pub submits: Option<u64>,
    /// Whether a synchronized DEC 2026 frame was still open at the read.
    pub frame_open: bool,
    /// Session input write count recorded before reading `rows`.
    pub inputs_before: Option<u64>,
    /// Session input write count recorded after reading `rows`.
    pub inputs_after: Option<u64>,
}

/// Person-initiated launch witness metadata from `text --json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchWitness {
    /// The command start it was recorded at, one per command line: a witness
    /// answers one box.
    pub id: u64,
    /// The person count recorded in the hold that parsed the command start.
    pub seq: u64,
    /// The command's first word: `claude` or `codex`.
    pub word: String,
}

/// Server-reported human input recency (`"human_ms"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HumanInput {
    /// Server did not report `"human_ms"`.
    #[default]
    Unknown,
    /// No human input since the host began serving the session.
    Never,
    /// No human input over the given host-served duration in milliseconds.
    NeverFor(u64),
    /// Milliseconds since the last human input before the read.
    Ago(u64),
}

impl HumanInput {
    /// Parse [`HumanInput`] from a `status` reply's `human_ms=` field.
    #[must_use]
    pub fn of_status(status: &str) -> Self {
        let Some(line) = status.lines().find(|l| crate::is_ok_reply(l)) else {
            return Self::Unknown;
        };
        match line
            .split_whitespace()
            .find_map(|w| w.strip_prefix("human_ms="))
        {
            None => Self::Unknown,
            Some("-") => Self::Never,
            Some(ms) => ms.parse().map_or(Self::Unknown, Self::Ago),
        }
    }

    /// Construct a never-input state from an optional `"unkeyed_ms"` duration.
    #[must_use]
    pub fn never(unkeyed_ms: Option<u64>) -> Self {
        unkeyed_ms.map_or(Self::Never, Self::NeverFor)
    }

    /// Age since the last human input, if [`HumanInput::Ago`].
    #[must_use]
    pub const fn ago(self) -> Option<Duration> {
        match self {
            Self::Ago(ms) => Some(Duration::from_millis(ms)),
            Self::Never | Self::NeverFor(_) | Self::Unknown => None,
        }
    }

    /// Whether human input occurred within `grace_s` seconds (`None` if [`HumanInput::Unknown`]).
    #[must_use]
    pub fn within(self, grace_s: u32) -> Option<bool> {
        match self {
            Self::Never | Self::NeverFor(_) => Some(false),
            Self::Ago(ms) => Some(ms < u64::from(grace_s).saturating_mul(1000)),
            Self::Unknown => None,
        }
    }

    /// Whether no human input occurred at or after `since_ms` for a read at `now_ms`.
    #[must_use]
    pub fn quiet_since(self, since_ms: u64, now_ms: u64) -> Option<bool> {
        match self {
            Self::Ago(ms) => Some(now_ms.saturating_sub(ms) < since_ms),
            Self::NeverFor(ms) => (now_ms.saturating_sub(ms) <= since_ms).then_some(true),
            Self::Never | Self::Unknown => None,
        }
    }
}

impl Screen {
    /// Copy cursor coordinates from `now` when `now` has the same `seq`, `cols`, and `pane_rows`.
    pub fn take_cursor_of(&mut self, now: &Screen) -> bool {
        if now.seq != self.seq || now.rows != self.rows {
            return false;
        }
        self.cursor_row = now.cursor_row;
        self.cursor_col = now.cursor_col;
        self.first = now.first;
        true
    }

    /// Index of the cursor row within `rows`, or `None` if outside the slice.
    pub fn cursor_index(&self) -> Option<usize> {
        self.cursor_row
            .checked_sub(self.first)
            .filter(|&i| i < self.rows.len())
    }
}

/// Parse the JSON payload of a `text --json` reply (with or without an `OK 1` header line).
pub fn parse_text_json(body: &str) -> Result<Screen, String> {
    let body = body.trim_start();
    let body = body
        .strip_prefix("OK 1")
        .map(str::trim_start)
        .unwrap_or(body);
    let mut v = Json::parse(body)?;
    let rows = match &mut v {
        Json::Object(kv) => kv
            .iter_mut()
            .find(|(k, _)| k == "rows")
            .map(|(_, r)| std::mem::replace(r, Json::Null)),
        _ => None,
    };
    let Some(Json::Array(rows)) = rows else {
        return Err("text --json: no rows array".to_string());
    };
    let rows = rows
        .into_iter()
        .map(|r| match r {
            Json::Str(s) => Ok(s),
            _ => Err("text --json: a row is not a string"),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let cursor = v.get("cursor");
    let num = |o: Option<&Json>, k: &str| o.and_then(|c| c.get(k)).and_then(Json::as_u64);
    let inputs = v
        .get("inputs")
        .and_then(Json::as_array)
        .filter(|i| i.len() == 2);
    Ok(Screen {
        rows,
        cursor_row: usize::try_from(num(cursor, "row").unwrap_or(0)).unwrap_or(usize::MAX),
        cursor_col: usize::try_from(num(cursor, "col").unwrap_or(0)).unwrap_or(usize::MAX),
        seq: num(Some(&v), "seq").unwrap_or(0),
        first: usize::try_from(num(Some(&v), "first").unwrap_or(0)).unwrap_or(usize::MAX),
        generation: v
            .get("gen")
            .and_then(Json::as_str)
            .filter(|g| ScreenGen::parse(g).is_some())
            .map(str::to_string),
        human: match v.get("human_ms") {
            None => HumanInput::Unknown,
            Some(Json::Null) => HumanInput::never(v.get("unkeyed_ms").and_then(Json::as_u64)),
            Some(h) => h.as_u64().map_or(HumanInput::Unknown, HumanInput::Ago),
        },
        human_seq: v.get("human_seq").and_then(Json::as_u64),
        cols: num(v.get("dims"), "cols").and_then(|c| u16::try_from(c).ok()),
        pane_rows: num(v.get("dims"), "rows").and_then(|r| usize::try_from(r).ok()),
        launch: v.get("launch").and_then(launch_witness),
        submits: v.get("submits").and_then(Json::as_u64),
        frame_open: v.get("frame").and_then(Json::as_str) == Some("open"),
        inputs_before: inputs.and_then(|i| i[0].as_u64()),
        inputs_after: inputs.and_then(|i| i[1].as_u64()),
    })
}

/// A `"launch"` value that says `person`, whole; anything else is none.
fn launch_witness(v: &Json) -> Option<LaunchWitness> {
    (v.get("why")?.as_str()? == "person").then_some(())?;
    Some(LaunchWitness {
        id: v.get("id")?.as_u64()?,
        seq: v.get("seq")?.as_u64()?,
        word: v.get("word")?.as_str()?.to_string(),
    })
}

/// Minimal JSON value representation.
#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Number(String),
    Str(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    /// Parse a JSON document into a [`Json`] value.
    pub fn parse(s: &str) -> Result<Json, String> {
        let mut i = 0;
        let v = parse_value(s, &mut i)?;
        skip_ws(s, &mut i);
        if i != s.len() {
            return Err(format!("json: trailing data at char {}", at(s, i)));
        }
        Ok(v)
    }

    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(kv) => kv.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub const fn as_array(&self) -> Option<&Vec<Json>> {
        match self {
            Json::Array(a) => Some(a),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Json::Number(n) => n.parse().ok(),
            _ => None,
        }
    }
}

/// The char index of byte offset `i`, as an error names it.
fn at(s: &str, i: usize) -> usize {
    s[..i].chars().count()
}

fn skip_ws(s: &str, i: &mut usize) {
    *i = s.len() - s[*i..].trim_start().len();
}

fn parse_value(s: &str, i: &mut usize) -> Result<Json, String> {
    skip_ws(s, i);
    let Some(ch) = s[*i..].chars().next() else {
        return Err("json: unexpected end".to_string());
    };
    match ch {
        '{' => {
            *i += 1;
            let mut kv = Vec::new();
            loop {
                skip_ws(s, i);
                if s[*i..].starts_with('}') {
                    *i += 1;
                    return Ok(Json::Object(kv));
                }
                if !kv.is_empty() {
                    expect(s, i, ',')?;
                    skip_ws(s, i);
                }
                let k = parse_string(s, i)?;
                skip_ws(s, i);
                expect(s, i, ':')?;
                let v = parse_value(s, i)?;
                kv.push((k, v));
            }
        }
        '[' => {
            *i += 1;
            let mut items = Vec::new();
            loop {
                skip_ws(s, i);
                if s[*i..].starts_with(']') {
                    *i += 1;
                    return Ok(Json::Array(items));
                }
                if !items.is_empty() {
                    expect(s, i, ',')?;
                }
                items.push(parse_value(s, i)?);
            }
        }
        '"' => Ok(Json::Str(parse_string(s, i)?)),
        't' => literal(s, i, "true", Json::Bool(true)),
        'f' => literal(s, i, "false", Json::Bool(false)),
        'n' => literal(s, i, "null", Json::Null),
        _ => {
            let start = *i;
            *i += s[start..]
                .bytes()
                .take_while(|&b| {
                    b.is_ascii_digit() || matches!(b, b'-' | b'+' | b'.' | b'e' | b'E')
                })
                .count();
            if start == *i {
                return Err(format!("json: unexpected {ch:?} at char {}", at(s, start)));
            }
            Ok(Json::Number(s[start..*i].to_string()))
        }
    }
}

fn expect(s: &str, i: &mut usize, want: char) -> Result<(), String> {
    if s[*i..].starts_with(want) {
        *i += want.len_utf8();
        Ok(())
    } else {
        Err(format!("json: expected {want:?} at char {}", at(s, *i)))
    }
}

fn literal(s: &str, i: &mut usize, word: &str, v: Json) -> Result<Json, String> {
    if s[*i..].starts_with(word) {
        *i += word.len();
        Ok(v)
    } else {
        Err(format!("json: bad literal at char {}", at(s, *i)))
    }
}

fn parse_string(s: &str, i: &mut usize) -> Result<String, String> {
    expect(s, i, '"')?;
    let mut out = String::new();
    while let Some((run, ch)) = s[*i..].char_indices().find(|&(_, c)| c == '"' || c == '\\') {
        out.push_str(&s[*i..*i + run]);
        *i += run + 1;
        if ch == '"' {
            return Ok(out);
        }
        let Some(e) = s[*i..].chars().next() else {
            break;
        };
        *i += e.len_utf8();
        match e {
            '"' => out.push('"'),
            '\\' => out.push('\\'),
            '/' => out.push('/'),
            'b' => out.push('\u{8}'),
            'f' => out.push('\u{c}'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            'u' => {
                let cp = hex4(s, i)?;
                let ch = if (0xD800..0xDC00).contains(&cp) && s[*i..].starts_with("\\u") {
                    *i += 2;
                    let lo = hex4(s, i)?;
                    let combined =
                        0x10000 + ((cp - 0xD800) << 10) + (lo.wrapping_sub(0xDC00) & 0x3FF);
                    char::from_u32(combined).unwrap_or('\u{FFFD}')
                } else {
                    char::from_u32(cp).unwrap_or('\u{FFFD}')
                };
                out.push(ch);
            }
            other => return Err(format!("json: bad escape \\{other}")),
        }
    }
    Err("json: unterminated string".to_string())
}

fn hex4(s: &str, i: &mut usize) -> Result<u32, String> {
    let rest = &s[*i..];
    let Some((last, c)) = rest.char_indices().nth(3) else {
        return Err("json: short \\u escape".to_string());
    };
    let t = &rest[..last + c.len_utf8()];
    *i += t.len();
    u32::from_str_radix(t, 16).map_err(|_| format!("json: bad \\u escape {t:?}"))
}

/// Whether a `cell` style reply includes the `dim` attribute; a cell of an unfinished frame (` frame=open`) is no evidence.
#[must_use]
pub fn cell_is_dim(line: &str) -> bool {
    aterm_codec::cell::parse_style(line).is_some_and(|style| style.dim)
}

/// Classify whether a `cell` reply represents a placeholder (`dim` or matching `hint_fg`).
#[must_use]
pub fn cell_is_placeholder(line: &str, hint_fg: Option<&str>) -> Option<bool> {
    let style = aterm_codec::cell::parse_style(line)?;
    if style.dim {
        return Some(true);
    }
    match hint_fg {
        None => Some(false),
        Some(hint) if aterm_codec::hex::is_hex(hint, 6) => {
            Some(style.fg.eq_ignore_ascii_case(hint))
        }
        Some(_) => None,
    }
}

/// Whether a `cell` reply's foreground color matches `fg` (case-insensitive hex).
#[must_use]
pub fn cell_fg_is(line: &str, fg: &str) -> bool {
    aterm_codec::cell::parse_style(line).is_some_and(|style| style.fg.eq_ignore_ascii_case(fg))
}

/// Return the lowercase foreground hex color of a `cell` reply when its glyph matches `glyph`.
#[must_use]
pub fn cell_glyph_fg(line: &str, glyph: char) -> Option<String> {
    let style = aterm_codec::cell::parse_style(line)?;
    let drawn = line.strip_prefix("OK ")?.split(' ').next()?;
    let mut want = [0; 4];
    (drawn == glyph.encode_utf8(&mut want)).then(|| style.fg.to_ascii_lowercase())
}

/// Extract `(glyph, style_rest)` from a well-formed non-blank `cell` reply.
#[must_use]
pub fn cell_drawn(line: &str) -> Option<(String, String)> {
    aterm_codec::cell::parse_style(line)?;
    let mut fields = line.trim_end().strip_prefix("OK ")?.split(' ');
    let glyph = fields.next()?.to_string();
    let style = fields.take(3).collect::<Vec<_>>().join(" ");
    Some((glyph, style))
}

/// Extract `<field>=<value>` from a `status` reply (`None` if absent or `"-"`).
pub fn status_field<'s>(stdout: &'s str, field: &str) -> Option<&'s str> {
    let line = stdout.lines().find(|l| crate::is_ok_reply(l))?;
    line.split_whitespace()
        .find_map(|w| w.strip_prefix(field)?.strip_prefix('='))
        .filter(|v| *v != "-")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`Screen::take_cursor_of`]: a later read of the same content gives its cursor whole (row,
    /// column and slice start); NEGATIVE CONTROLS: a read whose seq moved, or whose rows moved
    /// at the same seq, gives nothing, and the screen keeps the cursor it was read with.
    #[test]
    fn a_later_reads_cursor_is_taken_whole_only_over_the_same_content() {
        let point = Screen {
            rows: vec!["⏺ Done.".to_string(), "❯ ".to_string()],
            cursor_row: 10,
            cursor_col: 0,
            seq: 7,
            first: 0,
            ..Screen::default()
        };
        let caret = Screen {
            cursor_row: 3,
            cursor_col: 2,
            first: 2,
            ..point.clone()
        };
        let mut taken = point.clone();
        assert!(taken.take_cursor_of(&caret));
        assert_eq!((taken.cursor_row, taken.cursor_col, taken.first), (3, 2, 2));
        assert_eq!(taken.rows, point.rows);
        for moved in [
            Screen {
                seq: 8,
                ..caret.clone()
            },
            Screen {
                rows: vec!["⏺ Done.".to_string(), "❯ hello".to_string()],
                ..caret.clone()
            },
        ] {
            let mut kept = point.clone();
            assert!(!kept.take_cursor_of(&moved), "{moved:?}");
            assert_eq!(kept, point, "{moved:?}");
        }
    }

    /// A `status` reply's stamp reads as a `text --json` one does, and [`HumanInput::within`]
    /// is strictly under the grace.
    #[test]
    fn a_status_stamp_reads_as_the_one_person_fact() {
        let ok = "OK schema=1 hold=0 hand=- human_ms=5000 level=quiet";
        assert_eq!(HumanInput::of_status(ok), HumanInput::Ago(5000));
        assert_eq!(
            HumanInput::of_status(&ok.replace("5000", "-")),
            HumanInput::Never
        );
        assert_eq!(
            HumanInput::of_status("OK schema=1 hold=0"),
            HumanInput::Unknown,
            "an older server"
        );
        assert_eq!(
            HumanInput::of_status("ERR no such session human_ms=1"),
            HumanInput::Unknown
        );
        assert_eq!(
            HumanInput::of_status("OKAY human_ms=1"),
            HumanInput::Unknown
        );
        assert_eq!(status_field("OKAY human_ms=1", "human_ms"), None);
        assert_eq!(HumanInput::Ago(4999).within(5), Some(true));
        assert_eq!(HumanInput::Ago(5000).within(5), Some(false), "the edge");
        assert_eq!(HumanInput::Never.within(u32::MAX), Some(false));
        assert_eq!(HumanInput::Unknown.within(5), None);
        assert_eq!(HumanInput::Ago(u64::MAX).within(u32::MAX), Some(false));
    }

    /// The exact shape the server writes (see `cmd_text_json_opt`), with the
    /// header line the wire carries and without it (the client strips it).
    #[test]
    fn parses_the_servers_text_json_shape() {
        let body = r#"{"rows":["line-zero","second \"quoted\" \\ tab\t","❯ caf\u00e9 \ud83d\ude00"],"cursor":{"row":60,"col":2,"visible":true,"style":"blinking_block"},"dims":{"rows":63,"cols":138},"seq":122571}"#;
        for input in [body.to_string(), format!("OK 1\n{body}\n")] {
            let s = parse_text_json(&input).expect("parses");
            assert_eq!(s.rows.len(), 3);
            assert_eq!(s.rows[0], "line-zero");
            assert_eq!(s.rows[1], "second \"quoted\" \\ tab\t");
            assert_eq!(s.rows[2], "❯ café 😀");
            assert_eq!((s.cursor_row, s.cursor_col, s.seq), (60, 2, 122571));
            assert_eq!(s.cols, Some(138), "the pane's width, from dims");
            assert_eq!(
                s.pane_rows,
                Some(63),
                "the PANE's height, from dims — not the 3 rows sent"
            );
        }
        let bare = r#"{"rows":[],"cursor":{"row":0,"col":0},"seq":5}"#;
        assert_eq!(parse_text_json(bare).expect("parses").cols, None);
        assert_eq!(parse_text_json(bare).expect("parses").pane_rows, None);
        let huge =
            r#"{"rows":[],"cursor":{"row":0,"col":0},"dims":{"rows":2,"cols":70000},"seq":5}"#;
        assert_eq!(parse_text_json(huge).expect("parses").cols, None);
        let trimmed = r#"{"rows":[],"cursor":{"row":0,"col":0},"dims":{"rows":2,"cols":2},"seq":5,"trimmed":2}"#;
        let s = parse_text_json(trimmed).expect("parses");
        assert!(s.rows.is_empty());
        assert_eq!(s.seq, 5);
        assert_eq!(s.generation, None, "an older host sends no gen");
    }

    /// The screen generation (`"gen":"<epoch>.<seq>"`) is read as the fence a
    /// press names; a malformed one is not a fence at all.
    #[test]
    fn the_screen_generation_is_read_and_a_malformed_one_is_none() {
        let with = |g: &str| {
            parse_text_json(&format!(
                r#"{{"rows":[],"cursor":{{"row":0,"col":0}},"dims":{{"rows":2,"cols":2}},"seq":15,"gen":"{g}"}}"#
            ))
            .expect("parses")
            .generation
        };
        assert_eq!(with("2.15").as_deref(), Some("2.15"));
        for bad in [
            "15",
            "2.",
            ".15",
            "2.15.1",
            "-2.15",
            "a.b",
            "",
            "18446744073709551616.15",
        ] {
            assert_eq!(with(bad), None, "{bad:?}");
        }
    }

    /// `"human_ms"`: a number is its age, `null` is never, and absent or any other value is
    /// unknown.
    #[test]
    fn the_persons_stamp_is_read_and_its_absence_is_unknown() {
        let with = |tail: &str| {
            parse_text_json(&format!(
                r#"{{"rows":[],"cursor":{{"row":0,"col":0}},"dims":{{"rows":2,"cols":2}},"seq":15,"gen":"2.15"{tail}}}"#
            ))
            .expect("parses")
            .human
        };
        assert_eq!(with(r#","human_ms":12345"#), HumanInput::Ago(12_345));
        assert_eq!(with(r#","human_ms":0,"trimmed":1"#), HumanInput::Ago(0));
        assert_eq!(with(r#","human_ms":null"#), HumanInput::Never);
        assert_eq!(
            with(r#","human_ms":null,"trimmed":1,"unkeyed_ms":5000"#),
            HumanInput::NeverFor(5000),
            "a never that says since when"
        );
        assert_eq!(
            with(r#","human_ms":null,"unkeyed_ms":"5""#),
            HumanInput::Never
        );
        assert_eq!(with(""), HumanInput::Unknown, "an older host");
        assert_eq!(with(r#","human_ms":"12""#), HumanInput::Unknown);
        assert_eq!(with(r#","human_ms":-3"#), HumanInput::Unknown);
    }

    /// [`HumanInput::quiet_since`] (the review of LC-4, KR-2): an age is
    /// quiet when the key came before the instant; a never that says since
    /// when is quiet when the host served the session from the instant or
    /// before, and cannot say when it began after; a bare never and an
    /// unknown cannot say.
    #[test]
    fn a_never_that_says_since_when_is_quiet_from_before_the_instant() {
        let (since, now) = (1_000_000, 1_600_000);
        assert_eq!(HumanInput::Ago(700_000).quiet_since(since, now), Some(true));
        assert_eq!(
            HumanInput::Ago(500_000).quiet_since(since, now),
            Some(false)
        );
        assert_eq!(
            HumanInput::NeverFor(600_000).quiet_since(since, now),
            Some(true),
            "served from the instant"
        );
        assert_eq!(HumanInput::NeverFor(599_999).quiet_since(since, now), None);
        assert_eq!(HumanInput::Never.quiet_since(since, now), None);
        assert_eq!(HumanInput::Unknown.quiet_since(since, now), None);
        assert_eq!(HumanInput::NeverFor(1).within(u32::MAX), Some(false));
        assert_eq!(HumanInput::NeverFor(1).ago(), None);
    }

    /// `"submits"` (aterm-gui's `submit_count`, last in the object): a count
    /// is read; absent (an older host) and any other value are none.
    #[test]
    fn the_submission_count_is_read_and_its_absence_is_none() {
        let with = |tail: &str| {
            parse_text_json(&format!(
                r#"{{"rows":[],"cursor":{{"row":0,"col":0}},"dims":{{"rows":2,"cols":2}},"seq":15,"gen":"2.15","human_ms":null,"unkeyed_ms":5000{tail}}}"#
            ))
            .expect("parses")
            .submits
        };
        assert_eq!(with(r#","submits":7"#), Some(7));
        assert_eq!(with(r#","submits":0"#), Some(0));
        assert_eq!(with(""), None, "an older host");
        assert_eq!(with(r#","submits":"7""#), None);
        assert_eq!(with(r#","submits":-1"#), None);
    }

    /// `"frame":"open"` (aterm-gui's `text --json`, last in the object, only while the
    /// program's DEC 2026 frame is unfinished) is an open frame; absent (a whole frame, or an
    /// older host) and any other value are whole.
    #[test]
    fn an_open_frame_is_read_only_from_the_servers_open() {
        let with = |tail: &str| {
            parse_text_json(&format!(
                r#"{{"rows":["❯"],"cursor":{{"row":0,"col":2}},"dims":{{"rows":1,"cols":2}},"seq":15,"submits":3{tail}}}"#
            ))
            .expect("parses")
            .frame_open
        };
        assert!(with(r#","frame":"open""#));
        assert!(!with(""), "a whole frame, or an older host");
        assert!(!with(r#","frame":"whole""#));
        assert!(!with(r#","frame":true"#));
        assert!(!with(r#","frame":null"#));
    }

    /// `"inputs"` (aterm-gui's input writes, before and after the rows): both
    /// counts are read, a `null` first (a write in flight) is none before and
    /// the count after; absent (an older host) and any other shape are none.
    #[test]
    fn the_input_counts_are_read_and_a_write_in_flight_is_none_before() {
        let with = |tail: &str| {
            let s = parse_text_json(&format!(
                r#"{{"rows":[],"cursor":{{"row":0,"col":0}},"dims":{{"rows":2,"cols":2}},"seq":15,"gen":"2.15","human_ms":null{tail},"submits":1}}"#
            ))
            .expect("parses");
            (s.inputs_before, s.inputs_after)
        };
        assert_eq!(with(r#","inputs":[3,4]"#), (Some(3), Some(4)));
        assert_eq!(with(r#","inputs":[null,4]"#), (None, Some(4)));
        assert_eq!(with(""), (None, None), "an older host");
        assert_eq!(with(r#","inputs":[3]"#), (None, None));
        assert_eq!(with(r#","inputs":[3,4,5]"#), (None, None));
        assert_eq!(with(r#","inputs":7"#), (None, None));
        assert_eq!(with(r#","inputs":["3",-1]"#), (None, None));
    }

    /// `"launch"` (the server's launch witness, in the shape aterm-gui's
    /// `launch_witness` writes it): a `person` witness is read whole; every
    /// other word, `null`, a missing field and an older host that sends none
    /// are no witness.
    #[test]
    fn only_a_whole_person_launch_witness_is_read() {
        let with = |tail: &str| {
            parse_text_json(&format!(
                r#"{{"rows":[],"cursor":{{"row":0,"col":0}},"dims":{{"rows":2,"cols":2}},"seq":15,"gen":"2.15","human_ms":900,"human_seq":4{tail}}}"#
            ))
            .expect("parses")
            .launch
        };
        assert_eq!(
            with(r#","launch":{"id":3,"seq":4,"word":"claude","why":"person"},"trimmed":1"#),
            Some(LaunchWitness {
                id: 3,
                seq: 4,
                word: "claude".to_string()
            })
        );
        for other in [
            r#","launch":{"why":"since"}"#,
            r#","launch":{"why":"typed"}"#,
            r#","launch":{"id":3,"seq":4,"word":"claude","why":"pending"}"#,
            r#","launch":{"id":3,"word":"claude","why":"person"}"#,
            r#","launch":{"id":3,"seq":"4","word":"claude","why":"person"}"#,
            r#","launch":null"#,
            "",
        ] {
            assert_eq!(with(other), None, "{other}");
        }
    }

    /// A `tail=` read's `first` maps the cursor's grid row onto the rows read; a full read
    /// starts at 0, and a cursor above a tail is on none.
    #[test]
    fn a_tail_reads_first_row_places_the_cursor() {
        let tail = r#"{"rows":["a","❯ hi","b"],"cursor":{"row":61,"col":4},"dims":{"rows":63,"cols":80},"seq":9,"first":60}"#;
        let s = parse_text_json(tail).expect("parses");
        assert_eq!((s.first, s.cursor_row), (60, 61));
        assert_eq!(s.cursor_index(), Some(1));
        let full = r#"{"rows":["a","❯ hi"],"cursor":{"row":1,"col":4},"dims":{"rows":2,"cols":80},"seq":9}"#;
        let s = parse_text_json(full).expect("parses");
        assert_eq!((s.first, s.cursor_index()), (0, Some(1)));
        let above = Screen {
            rows: vec!["a".to_string()],
            cursor_row: 3,
            first: 10,
            ..Screen::default()
        };
        assert_eq!(above.cursor_index(), None);
    }

    #[test]
    fn rejects_what_is_not_a_screen() {
        assert!(parse_text_json("ERR usage: text").is_err());
        assert!(parse_text_json(r#"{"rows":"no"}"#).is_err());
        assert!(parse_text_json(r#"{"rows":[1]}"#).is_err());
        assert!(parse_text_json(r#"{"rows":[]} x"#).is_err());
        assert!(parse_text_json("").is_err());
    }

    #[test]
    fn json_reader_handles_the_value_vocabulary() {
        let v = Json::parse(r#" {"a":[true,false,null,-1.5e3,"x"],"b":{}} "#).expect("parses");
        let a = v.get("a").and_then(Json::as_array).expect("array");
        assert_eq!(a[0], Json::Bool(true));
        assert_eq!(a[2], Json::Null);
        assert_eq!(a[3], Json::Number("-1.5e3".to_string()));
        assert_eq!(a[4].as_str(), Some("x"));
        assert_eq!(v.get("b"), Some(&Json::Object(vec![])));
        assert!(Json::parse("[1,]").is_err());
        assert!(Json::parse("{\"a\" 1}").is_err());
        assert!(Json::parse("tru").is_err());
        assert!(Json::parse("\"\\q\"").is_err());
    }

    /// [`cell_fg_is`] reads a well-formed cell's colour; a cell of an unfinished frame is no evidence.
    #[test]
    fn a_cell_reply_names_its_colour_only_off_a_whole_frame() {
        assert!(cell_fg_is("OK x 8A8F99 111318 none", "8a8f99"));
        assert!(!cell_fg_is("OK x d0d0d0 111318 none", "8a8f99"));
        assert!(!cell_fg_is("OK x 8a8f99 111318 none frame=open", "8a8f99"));
        assert!(!cell_fg_is("ERR out of range", "8a8f99"));
    }
}
