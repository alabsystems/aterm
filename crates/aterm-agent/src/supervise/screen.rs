// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `text --json` reply as the supervisor reads it: the rows, the cursor and
//! the content sequence. A small std-only JSON reader — the reply is one object
//! of known shape (`{"rows":[…],"cursor":{"row":r,"col":c,…},"dims":{…},"seq":n}`)
//! and this crate's closure stays free of a serde stack for it.

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
}

/// Parse the JSON body of a `text --json` reply (with or without the `OK 1`
/// header line the wire carries; the CLI client strips it).
pub fn parse_text_json(body: &str) -> Result<Screen, String> {
    let body = body.trim_start();
    let body = body
        .strip_prefix("OK 1")
        .map(str::trim_start)
        .unwrap_or(body);
    let v = Json::parse(body)?;
    let rows = v
        .get("rows")
        .and_then(Json::as_array)
        .ok_or("text --json: no rows array")?
        .iter()
        .map(|r| {
            r.as_str()
                .map(str::to_string)
                .ok_or("text --json: a row is not a string")
        })
        .collect::<Result<Vec<_>, _>>()?;
    let cursor = v.get("cursor");
    let num = |o: Option<&Json>, k: &str| o.and_then(|c| c.get(k)).and_then(Json::as_u64);
    Ok(Screen {
        rows,
        cursor_row: usize::try_from(num(cursor, "row").unwrap_or(0)).unwrap_or(usize::MAX),
        cursor_col: usize::try_from(num(cursor, "col").unwrap_or(0)).unwrap_or(usize::MAX),
        seq: num(Some(&v), "seq").unwrap_or(0),
    })
}

/// Parse a plain `text` reply (one row per line) when `--json` is unavailable.
pub fn parse_text_plain(body: &str) -> Screen {
    Screen {
        rows: body.lines().map(str::to_string).collect(),
        ..Screen::default()
    }
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
        let chars: Vec<char> = s.chars().collect();
        let mut i = 0;
        let v = parse_value(&chars, &mut i)?;
        skip_ws(&chars, &mut i);
        if i != chars.len() {
            return Err(format!("json: trailing data at char {i}"));
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

fn skip_ws(c: &[char], i: &mut usize) {
    while *i < c.len() && c[*i].is_whitespace() {
        *i += 1;
    }
}

fn parse_value(c: &[char], i: &mut usize) -> Result<Json, String> {
    skip_ws(c, i);
    let Some(&ch) = c.get(*i) else {
        return Err("json: unexpected end".to_string());
    };
    match ch {
        '{' => {
            *i += 1;
            let mut kv = Vec::new();
            loop {
                skip_ws(c, i);
                if c.get(*i) == Some(&'}') {
                    *i += 1;
                    return Ok(Json::Object(kv));
                }
                if !kv.is_empty() {
                    expect(c, i, ',')?;
                    skip_ws(c, i);
                }
                let k = parse_string(c, i)?;
                skip_ws(c, i);
                expect(c, i, ':')?;
                let v = parse_value(c, i)?;
                kv.push((k, v));
            }
        }
        '[' => {
            *i += 1;
            let mut items = Vec::new();
            loop {
                skip_ws(c, i);
                if c.get(*i) == Some(&']') {
                    *i += 1;
                    return Ok(Json::Array(items));
                }
                if !items.is_empty() {
                    expect(c, i, ',')?;
                }
                items.push(parse_value(c, i)?);
            }
        }
        '"' => Ok(Json::Str(parse_string(c, i)?)),
        't' => literal(c, i, "true", Json::Bool(true)),
        'f' => literal(c, i, "false", Json::Bool(false)),
        'n' => literal(c, i, "null", Json::Null),
        _ => {
            let start = *i;
            while *i < c.len()
                && (c[*i].is_ascii_digit() || matches!(c[*i], '-' | '+' | '.' | 'e' | 'E'))
            {
                *i += 1;
            }
            if start == *i {
                return Err(format!("json: unexpected {ch:?} at char {start}"));
            }
            Ok(Json::Number(c[start..*i].iter().collect()))
        }
    }
}

fn expect(c: &[char], i: &mut usize, want: char) -> Result<(), String> {
    if c.get(*i) == Some(&want) {
        *i += 1;
        Ok(())
    } else {
        Err(format!("json: expected {want:?} at char {}", *i))
    }
}

fn literal(c: &[char], i: &mut usize, word: &str, v: Json) -> Result<Json, String> {
    let n = word.chars().count();
    if c.len() >= *i + n && c[*i..*i + n].iter().copied().eq(word.chars()) {
        *i += n;
        Ok(v)
    } else {
        Err(format!("json: bad literal at char {}", *i))
    }
}

fn parse_string(c: &[char], i: &mut usize) -> Result<String, String> {
    expect(c, i, '"')?;
    let mut out = String::new();
    while let Some(&ch) = c.get(*i) {
        *i += 1;
        match ch {
            '"' => return Ok(out),
            '\\' => {
                let Some(&e) = c.get(*i) else {
                    break;
                };
                *i += 1;
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
                        let cp = hex4(c, i)?;
                        // A surrogate pair arrives as two escapes.
                        let ch = if (0xD800..0xDC00).contains(&cp)
                            && c.get(*i) == Some(&'\\')
                            && c.get(*i + 1) == Some(&'u')
                        {
                            *i += 2;
                            let lo = hex4(c, i)?;
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
            _ => out.push(ch),
        }
    }
    Err("json: unterminated string".to_string())
}

fn hex4(c: &[char], i: &mut usize) -> Result<u32, String> {
    if c.len() < *i + 4 {
        return Err("json: short \\u escape".to_string());
    }
    let s: String = c[*i..*i + 4].iter().collect();
    *i += 4;
    u32::from_str_radix(&s, 16).map_err(|_| format!("json: bad \\u escape {s:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

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
        }
        // The trimmed form adds a field; unknown fields are ignored.
        let trimmed = r#"{"rows":[],"cursor":{"row":0,"col":0},"dims":{"rows":2,"cols":2},"seq":5,"trimmed":2}"#;
        let s = parse_text_json(trimmed).expect("parses");
        assert!(s.rows.is_empty());
        assert_eq!(s.seq, 5);
    }

    #[test]
    fn rejects_what_is_not_a_screen() {
        assert!(parse_text_json("ERR usage: text").is_err());
        assert!(parse_text_json(r#"{"rows":"no"}"#).is_err());
        assert!(parse_text_json(r#"{"rows":[1]}"#).is_err());
        assert!(parse_text_json(r#"{"rows":[]} x"#).is_err());
        assert!(parse_text_json("").is_err());
        let s = parse_text_plain("a\nb\n");
        assert_eq!(s.rows, vec!["a".to_string(), "b".to_string()]);
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
