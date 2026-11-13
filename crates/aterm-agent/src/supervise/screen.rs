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
    /// Server-reported timestamp of the person's last typing (`"human_typed_ms"`: a key, text,
    /// a key sequence or a paste that reached the program, never a scroll or a click); where the
    /// reply leaves it out, [`Self::human`], as a window that reports no typing stamp of its own
    /// says it.
    pub typed: HumanInput,
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
    /// The program's OSC 0/2 title (`"title"`), read in the hold that read `rows`: `Some("")` when the
    /// program set none, `None` from a host (or a recorded tape) whose reply has no such string, so
    /// neither says anything about the program. It is the last title set in the tab, which outlives
    /// the program that set it, so it speaks only beside rows that program draws.
    pub title: Option<String>,
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
            Some(ms) => crate::parse_uint(ms).map_or(Self::Unknown, Self::Ago),
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

    /// [`cursor_under_composer`] of this read's cursor, the composer's caret on row `caret`.
    #[must_use]
    pub fn cursor_under(&self, caret: usize) -> bool {
        cursor_under_composer(&self.rows, self.cursor_index(), caret)
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
            .rfind(|(k, _)| k == "rows")
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
    let human = match v.get("human_ms") {
        None => HumanInput::Unknown,
        Some(Json::Null) => HumanInput::never(v.get("unkeyed_ms").and_then(Json::as_u64)),
        Some(h) => h.as_u64().map_or(HumanInput::Unknown, HumanInput::Ago),
    };
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
        human,
        typed: match v.get("human_typed_ms") {
            None => human,
            Some(Json::Null) => HumanInput::Never,
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
        title: v.get("title").and_then(Json::as_str).map(str::to_string),
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
            Json::Object(kv) => kv.iter().rfind(|(k, _)| k == key).map(|(_, v)| v),
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
                let save = *i;
                let ch = if (0xD800..0xDC00).contains(&cp) && s[*i..].starts_with("\\u") {
                    *i += 2;
                    let lo = hex4(s, i)?;
                    if (0xDC00..0xE000).contains(&lo) {
                        let combined = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                        char::from_u32(combined).unwrap_or('\u{FFFD}')
                    } else {
                        *i = save;
                        '\u{FFFD}'
                    }
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
    if !t.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("json: bad \\u escape {t:?}"));
    }
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

/// THE CELL PAST A DRAWN CARET: when the `cell` reply at `col` is an `inverse` glyph — a caret the
/// program draws itself, over text it may type or may only suggest — the column of the glyph
/// after it, whose cell says which the text is; `None` when the reply's own cell says so.
/// Claude Code 2.1.310 draws its grey suggestion's first glyph inverse under its caret and the
/// rest `dim` (measured 2026-10-24 in the real-Claude lab), and a typed draft's first glyph the
/// same way with the rest plain. A `wide` glyph spans two columns; a reply of an unfinished frame
/// (` frame=open`) is no caret.
#[must_use]
pub fn past_drawn_caret(line: &str, col: usize) -> Option<usize> {
    let style = aterm_codec::cell::parse_style(line)?;
    style
        .inverse
        .then_some(col + if style.wide { 2 } else { 1 })
}

/// The column a composer's first glyph is drawn at, after its `❯ `.
pub const COMPOSER_COL: usize = 2;

/// WHETHER A CURSOR AT `cursor` (row and column; `None` where the read has none on its rows, as one
/// above them) STANDS HOME ON A COMPOSER OF `lines` FROM ROW `caret`: at the column its first glyph is
/// drawn at, where a program that draws grey text leaves its cursor over it. That is the caret's row,
/// or, on a lone line, the row under it, where a grid a resize left drawn one row high has it
/// (`docs/DESIGN-wall-resume-2026-09-28.md`). A later row of the composer is where only text of more
/// than one row, or a redraw not yet done, leaves it, and any other row, or none, is a frame read
/// before its program placed the cursor.
///
/// With the glyph's cell ([`composer_glyph_cell`]) this is what the loop's `composer_draft` and
/// Claude Code's and Muse Code's upgrade gates read grey text by: a suggestion only here.
#[must_use]
pub fn cursor_home(cursor: Option<(usize, usize)>, caret: usize, lines: &[String]) -> bool {
    cursor.is_some_and(|(row, col)| {
        col == COMPOSER_COL && (row == caret || (lines.len() == 1 && row == caret + 1))
    })
}

/// WHETHER THE CURSOR STANDS UNDER THE COMPOSER whose caret is on row `caret` of `rows`: a rule
/// between that row and the cursor's, so past the composer's bottom rule (on the rule itself it is a
/// displaced grid's, [`cursor_home`]). Of a live composer's states measured only a program's
/// prompt-history search (Ctrl-R) leaves it there, on the search's own row: Claude Code 2.1.318's
/// at every width from 144 columns down to 12 (`claude-2.1.318-history-search*`), and Muse Code
/// 1.4.14's; its other states (the `/`, `@`, `?`, `!` and `#` modes, Ctrl-C, Esc Esc, Shift+Tab,
/// Ctrl-T, Ctrl-O) leave it on the composer (docs/measured/draft-hold-findings-2026-11-04.md). It
/// also stands there under the old prompt box a relaunch left drawn above its new launch line
/// (`claude-2.1.283-inline-relaunch-before-repl`), and in a read taken inside a redraw before the
/// program placed its cursor (`half-drawn-frame-2026-10-22.md`). Keys go where the cursor stands,
/// so the composer's rows say nothing of a draft: no empty composer to type into, no draft an
/// Enter sends.
#[must_use]
pub fn cursor_under_composer(rows: &[String], cursor_row: Option<usize>, caret: usize) -> bool {
    cursor_row.is_some_and(|row| {
        rows.get(caret + 1..row)
            .is_some_and(|between| between.iter().any(|r| aterm_phase::phase::is_rule(r)))
    })
}

/// WHETHER THE FIRST GLYPH'S CELL DECIDES [`composer_holds_no_draft`] for a composer of `lines` from
/// row `caret`: text on its first line, the cursor home on it ([`cursor_home`]). Anywhere else the
/// rows decide alone, so a gate that reads the cell before it judges asks it only here.
#[must_use]
pub fn grey_decides(cursor: Option<(usize, usize)>, caret: usize, lines: &[String]) -> bool {
    lines.first().is_some_and(|l| !l.is_empty()) && cursor_home(cursor, caret, lines)
}

/// WHETHER A COMPOSER OF `lines` FROM ROW `caret` OF `rows` HOLDS NO DRAFT, as Claude Code's and Muse
/// Code's upgrade gates read it (the loop's `composer_draft` by the same rule): the cursor on or above
/// its bottom rule ([`cursor_under_composer`]: under it, a search's keys go elsewhere), and nothing
/// in it, or a first line that `grey` says is drawn grey with the cursor home. A blank first line
/// over text on a later row is text, whatever a blank's cell is drawn in, so `grey` is asked only
/// where it decides ([`grey_decides`]).
#[must_use]
pub fn composer_holds_no_draft(
    rows: &[String],
    cursor: Option<(usize, usize)>,
    caret: usize,
    lines: &[String],
    grey: impl FnOnce() -> bool,
) -> bool {
    if cursor_under_composer(rows, cursor.map(|(row, _)| row), caret) {
        false
    } else if grey_decides(cursor, caret, lines) {
        grey()
    } else {
        lines.iter().all(String::is_empty)
    }
}

/// THE COMPOSER'S FIRST GLYPH, READ WHERE ITS CELL SAYS WHAT THE TEXT IS: `read(col)` is the
/// composer row's `cell` reply at `col` (`None` where none came). Column [`COMPOSER_COL`], or, where
/// the program draws its own caret over that glyph, the glyph after it ([`past_drawn_caret`]).
/// The one read behind the loop's `composer_draft` and Claude Code's and Muse Code's upgrade
/// gates ([`cursor_home`]).
pub fn composer_glyph_cell(mut read: impl FnMut(usize) -> Option<String>) -> Option<String> {
    let first = read(COMPOSER_COL)?;
    match past_drawn_caret(&first, COMPOSER_COL) {
        None => Some(first),
        Some(col) => read(col),
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
#[path = "screen_tests.rs"]
mod tests;
