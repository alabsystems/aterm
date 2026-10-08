// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `text --json` reply as the supervisor reads it: the rows, the cursor, the
//! content sequence, the screen generation and the person's stamp. A small
//! std-only JSON reader — the reply is one object of known shape (`{"rows":[…],
//! "cursor":{"row":r,"col":c,…},"dims":{…},"seq":n,"gen":"e.s","human_ms":n|null}`)
//! and this crate's closure stays free of a serde stack for it.

use aterm_types::control_verbs::ScreenGen;

/// One screen read.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Screen {
    /// The visible rows, top to bottom (only the tail when `tail=` was used).
    pub rows: Vec<String>,
    /// Cursor row (0-based, in the full grid).
    pub cursor_row: usize,
    /// Cursor column (0-based).
    pub cursor_col: usize,
    /// The content sequence the read was taken at (`await seq <n>` waits past it).
    pub seq: u64,
    /// The grid row the first of `rows` is: a shaped read's `first` (`tail=`
    /// sends it when it does not start at row 0), 0 for a full read.
    pub first: usize,
    /// The screen generation the read was taken at, `<epoch>.<seq>` (the
    /// server's `"gen"`, read under the same lock as the rows): what a fenced
    /// press names (`key if-gen=`). Unlike `seq` it never repeats, so a press
    /// fenced on it cannot land on a box that replaced the one read. `None`
    /// from a host that does not send it (the press then carries no fence).
    pub generation: Option<String>,
    /// When a person last gave the session input, as the server stamped it at
    /// the read (`"human_ms"`): what the question answer waits on before it
    /// keys a dialog a person may be navigating.
    pub human: HumanInput,
    /// How many person gestures had reached the session at the read
    /// (`"human_seq"`): what a key fenced `if-human=` names, so the server
    /// writes it only if no person has keyed since. `None` from a host that
    /// does not send it (the key then carries no person fence).
    pub human_seq: Option<u64>,
    /// The pane's width in cells (`"dims":{"cols":…}`), `None` from a reply
    /// that does not carry it: what a question's policy text is estimated
    /// to wrap in.
    pub cols: Option<u16>,
    /// The PANE's height in rows (`"dims":{"rows":…}`: the grid's, whatever
    /// slice `tail=` sent in [`Self::rows`]), `None` from a reply that does
    /// not carry it: what a question's policy text is estimated to fit in.
    pub pane_rows: Option<usize>,
}

/// The server's person stamp on a read ([`Screen::human`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HumanInput {
    /// The host does not send it: unknown, and the question answer then waits
    /// for the screen to hold still instead.
    #[default]
    Unknown,
    /// No person has given the session input since the host began serving it.
    Never,
    /// This many milliseconds before the read.
    Ago(u64),
}

impl HumanInput {
    /// The stamp of one `status` reply (`human_ms=<ms|->`): `-` is
    /// [`Self::Never`], a count its age, and no such field — a server older
    /// than the stamp, or any reply but an `OK` — [`Self::Unknown`].
    #[must_use]
    pub fn of_status(status: &str) -> Self {
        let Some(line) = status.lines().find(|l| l.starts_with("OK")) else {
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

    /// Whether a person gave the session input within `grace_s` seconds
    /// (`[harness] human_grace_s`): the one test of a person's hand, for
    /// every caller. `None` for [`Self::Unknown`] — each caller says what an
    /// unknown stamp means where it acts (nobody, for a relaunch; someone,
    /// for the upgrade's notice).
    #[must_use]
    pub fn within(self, grace_s: u32) -> Option<bool> {
        match self {
            Self::Never => Some(false),
            Self::Ago(ms) => Some(ms < u64::from(grace_s).saturating_mul(1000)),
            Self::Unknown => None,
        }
    }

    /// Whether NO person gave the session input at or after `since_ms` (Unix
    /// milliseconds), for a stamp read at `now_ms`: an age is true where the
    /// input it dates came before `since_ms`; [`Self::Unknown`] AND
    /// [`Self::Never`] are `None`, for the caller to read as someone. Never
    /// says nobody keyed the session since the host BEGAN SERVING it, which
    /// may be after `since_ms`: the stamp lives in the serving process, and a
    /// seamless update's successor reads every session it adopts as never
    /// keyed — a person's keys before the update are gone from it, not
    /// absent. The instant is `now_ms − age`, so a caller's clock read after
    /// the server's stamp dates the input LATER than it was — the safe way
    /// to be wrong.
    #[must_use]
    pub fn quiet_since(self, since_ms: u64, now_ms: u64) -> Option<bool> {
        match self {
            Self::Ago(ms) => Some(now_ms.saturating_sub(ms) < since_ms),
            Self::Never | Self::Unknown => None,
        }
    }
}

impl Screen {
    /// The cursor's index into `rows` — its grid row less [`Self::first`];
    /// `None` when that row is not among the rows read.
    pub fn cursor_index(&self) -> Option<usize> {
        self.cursor_row
            .checked_sub(self.first)
            .filter(|&i| i < self.rows.len())
    }
}

/// Parse the JSON body of a `text --json` reply (with or without the `OK 1`
/// header line the wire carries; the CLI client strips it).
pub fn parse_text_json(body: &str) -> Result<Screen, String> {
    let body = body.trim_start();
    let body = body
        .strip_prefix("OK 1")
        .map(str::trim_start)
        .unwrap_or(body);
    let mut v = Json::parse(body)?;
    // The rows are MOVED out of the parsed value, never copied: nothing below
    // reads the top-level `rows` again.
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
    Ok(Screen {
        rows,
        cursor_row: usize::try_from(num(cursor, "row").unwrap_or(0)).unwrap_or(usize::MAX),
        cursor_col: usize::try_from(num(cursor, "col").unwrap_or(0)).unwrap_or(usize::MAX),
        seq: num(Some(&v), "seq").unwrap_or(0),
        first: usize::try_from(num(Some(&v), "first").unwrap_or(0)).unwrap_or(usize::MAX),
        // Kept only if the server will take it back as a fence: asked of the
        // same reader the server parses `if-gen=` with. Anything else is not
        // sent (the server would answer `ERR usage`); a local "two runs of
        // digits" check stood here and passed an epoch past `u64::MAX`.
        generation: v
            .get("gen")
            .and_then(Json::as_str)
            .filter(|g| ScreenGen::parse(g).is_some())
            .map(str::to_string),
        // Absent (an older host) is unknown; `null` is never; a number is
        // its age. Anything else is unknown, never a guess.
        human: match v.get("human_ms") {
            None => HumanInput::Unknown,
            Some(Json::Null) => HumanInput::Never,
            Some(h) => h.as_u64().map_or(HumanInput::Unknown, HumanInput::Ago),
        },
        human_seq: v.get("human_seq").and_then(Json::as_u64),
        cols: num(v.get("dims"), "cols").and_then(|c| u16::try_from(c).ok()),
        pane_rows: num(v.get("dims"), "rows").and_then(|r| usize::try_from(r).ok()),
    })
}

/// A minimal JSON value.
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
    /// Parse one JSON document.
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

    pub fn as_array(&self) -> Option<&Vec<Json>> {
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

// The reader walks the `&str` itself: `i` is a BYTE offset, always on a char
// boundary, and an error names the CHAR index (`at`).

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
    // The plain run up to the next quote or escape is copied in one step.
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
                // A surrogate pair arrives as two escapes.
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

/// Whether one `cell` reply (`OK <grapheme%enc> <fg> <bg> <attrs>[ link=…]`,
/// the attrs a comma list or `none`; aterm-gui `cmd_cell`) names `dim`. The
/// fields are split on single spaces, never on runs: a blank cell's grapheme
/// is an empty token. This is the one reading of a composer's column-2 cell,
/// telling the dim placeholder from a typed draft whose caret was moved home;
/// the supervisor's loop and the live upgrade's gate both ask it.
#[must_use]
pub fn cell_is_dim(line: &str) -> bool {
    line.trim_end()
        .strip_prefix("OK ")
        .and_then(|rest| rest.split(' ').nth(3))
        .is_some_and(|attrs| attrs.split(',').any(|a| a == "dim"))
}

/// The `<field>=` word of a `status` reply (`-` and absent are `None`).
pub fn status_field<'s>(stdout: &'s str, field: &str) -> Option<&'s str> {
    let line = stdout.lines().find(|l| l.starts_with("OK"))?;
    line.split_whitespace()
        .find_map(|w| w.strip_prefix(field)?.strip_prefix('='))
        .filter(|v| *v != "-")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `status` reply's stamp reads as a `text --json` one does: a count
    /// its age, `-` never, and no field (an older server) or no `OK`
    /// unknown — and [`HumanInput::within`] is the one grace test, strictly
    /// under it. Negative controls: the grace's own edge is outside it, and
    /// `human_ms=` on an `ERR` is no stamp.
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
        // No dims, or a width no pane has: unknown, never a guess.
        let bare = r#"{"rows":[],"cursor":{"row":0,"col":0},"seq":5}"#;
        assert_eq!(parse_text_json(bare).expect("parses").cols, None);
        assert_eq!(parse_text_json(bare).expect("parses").pane_rows, None);
        let huge =
            r#"{"rows":[],"cursor":{"row":0,"col":0},"dims":{"rows":2,"cols":70000},"seq":5}"#;
        assert_eq!(parse_text_json(huge).expect("parses").cols, None);
        // The trimmed form adds a field; unknown fields are ignored.
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

    /// The person's stamp (`"human_ms"`): a number is its age, `null` is
    /// never, and a host that sends none is unknown, as is a value that is
    /// neither.
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
        assert_eq!(with(""), HumanInput::Unknown, "an older host");
        assert_eq!(with(r#","human_ms":"12""#), HumanInput::Unknown);
        assert_eq!(with(r#","human_ms":-3"#), HumanInput::Unknown);
    }

    /// A `tail=` read that does not start at row 0 says where it starts
    /// (`first`), and the cursor's grid row maps onto the rows read through
    /// it; a full read starts at 0, and a cursor above a tail is on none.
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
}
