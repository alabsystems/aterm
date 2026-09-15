// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A minimal JSON value — parser and pretty-printer — for the ONE JSON document
//! this crate edits: an agent vendor's settings file (`hook install --merge`).
//!
//! Written out rather than depended on, for the reason `hook.rs`'s
//! `json_string` already gave: `aterm-link`'s dependency set is pinned by
//! `DESIGN-aterm-fabric.md` §11.2 and a serializer is not on it. What a merge
//! needs is small — read a document, keep every key it does not own, replace
//! the entries it does, write it back — and every part of that is here, with
//! nothing clever: a recursive-descent parser with a depth bound, and a
//! printer that emits the two-space form the vendor's own writer uses.
//!
//! **Keys keep their order and numbers keep their text.** An object is a
//! `Vec<(String, Json)>`, not a map, so a merged file reads back in the order
//! its owner wrote it; a number is the literal that was parsed, so `1.0` does
//! not come back as `1` and `1e3` does not come back as `1000`. The merge
//! touches one key (`hooks`) and must be able to say, of every other byte of
//! meaning in the file, that it is still there.

use std::fmt::Write as _;

/// The most nested a document may be before the parser refuses it. A settings
/// file is three or four levels deep; this is a bound against a crafted file
/// exhausting the stack, not a limit anyone will meet.
const DEPTH_MAX: usize = 64;

/// One JSON value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Json {
    Null,
    Bool(bool),
    /// The number's own text, validated against the JSON grammar and kept as
    /// written.
    Number(String),
    Str(String),
    Array(Vec<Json>),
    /// Members in source order; a duplicate key is kept as parsed and
    /// [`Json::get`] answers the FIRST.
    Object(Vec<(String, Json)>),
}

impl Json {
    /// Parse one document. Leading and trailing whitespace is allowed; anything
    /// else after the value is an error.
    ///
    /// # Errors
    ///
    /// The byte offset and a word about what was expected.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut p = Parser {
            src: text.as_bytes(),
            at: 0,
        };
        p.ws();
        let value = p.value(0)?;
        p.ws();
        if p.at != p.src.len() {
            return Err(p.err("end of document"));
        }
        Ok(value)
    }

    /// The member `key` of an object, or `None` for a missing key or a
    /// non-object.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Self> {
        match self {
            Self::Object(members) => members.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// [`Json::get`], mutably.
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Self> {
        match self {
            Self::Object(members) => members.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The member `key` of an object, inserted as `default` when absent.
    /// `None` when `self` is not an object.
    pub fn entry(&mut self, key: &str, default: Self) -> Option<&mut Self> {
        let Self::Object(members) = self else {
            return None;
        };
        if !members.iter().any(|(k, _)| k == key) {
            members.push((key.to_string(), default));
        }
        members.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_array(&self) -> Option<&[Self]> {
        match self {
            Self::Array(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_array_mut(&mut self) -> Option<&mut Vec<Self>> {
        match self {
            Self::Array(items) => Some(items),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_object(&self) -> Option<&[(String, Self)]> {
        match self {
            Self::Object(members) => Some(members),
            _ => None,
        }
    }

    pub fn as_object_mut(&mut self) -> Option<&mut Vec<(String, Self)>> {
        match self {
            Self::Object(members) => Some(members),
            _ => None,
        }
    }

    /// The document, pretty-printed with two-space indentation and no trailing
    /// newline — the form the vendor's own settings writer emits, so a merged
    /// file diffs against what was there by the lines that changed.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        self.render_into(&mut out, 0);
        out
    }

    fn render_into(&self, out: &mut String, depth: usize) {
        match self {
            Self::Null => out.push_str("null"),
            Self::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Self::Number(n) => out.push_str(n),
            Self::Str(s) => out.push_str(&string(s)),
            Self::Array(items) => {
                if items.is_empty() {
                    out.push_str("[]");
                    return;
                }
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    out.push_str(if i == 0 { "\n" } else { ",\n" });
                    indent(out, depth + 1);
                    item.render_into(out, depth + 1);
                }
                out.push('\n');
                indent(out, depth);
                out.push(']');
            }
            Self::Object(members) => {
                if members.is_empty() {
                    out.push_str("{}");
                    return;
                }
                out.push('{');
                for (i, (k, v)) in members.iter().enumerate() {
                    out.push_str(if i == 0 { "\n" } else { ",\n" });
                    indent(out, depth + 1);
                    out.push_str(&string(k));
                    out.push_str(": ");
                    v.render_into(out, depth + 1);
                }
                out.push('\n');
                indent(out, depth);
                out.push('}');
            }
        }
    }
}

fn indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

/// One JSON string literal, quoted and escaped.
#[must_use]
pub fn string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

struct Parser<'a> {
    src: &'a [u8],
    at: usize,
}

impl Parser<'_> {
    fn err(&self, want: &str) -> String {
        format!("byte {}: expected {want}", self.at)
    }

    fn ws(&mut self) {
        while self.at < self.src.len() && matches!(self.src[self.at], b' ' | b'\t' | b'\n' | b'\r')
        {
            self.at += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.at).copied()
    }

    fn eat(&mut self, want: u8) -> Result<(), String> {
        if self.peek() == Some(want) {
            self.at += 1;
            Ok(())
        } else {
            Err(self.err(&format!("`{}`", want as char)))
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, String> {
        if depth > DEPTH_MAX {
            return Err(self.err("a document nested less than 64 deep"));
        }
        match self.peek() {
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => self.string().map(Json::Str),
            Some(b't') => self.literal("true", Json::Bool(true)),
            Some(b'f') => self.literal("false", Json::Bool(false)),
            Some(b'n') => self.literal("null", Json::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => Err(self.err("a value")),
        }
    }

    fn literal(&mut self, word: &str, value: Json) -> Result<Json, String> {
        if self.src[self.at..].starts_with(word.as_bytes()) {
            self.at += word.len();
            Ok(value)
        } else {
            Err(self.err(word))
        }
    }

    fn number(&mut self) -> Result<Json, String> {
        let start = self.at;
        if self.peek() == Some(b'-') {
            self.at += 1;
        }
        match self.peek() {
            Some(b'0') => self.at += 1,
            Some(b'1'..=b'9') => self.digits(),
            _ => return Err(self.err("a digit")),
        }
        if self.peek() == Some(b'.') {
            self.at += 1;
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.err("a digit after `.`"));
            }
            self.digits();
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.at += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.at += 1;
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.err("an exponent"));
            }
            self.digits();
        }
        // The slice is ASCII by construction: every byte stepped over above
        // is a digit, a sign, a dot or an `e`.
        Ok(Json::Number(
            String::from_utf8_lossy(&self.src[start..self.at]).into_owned(),
        ))
    }

    fn digits(&mut self) {
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.at += 1;
        }
    }

    fn string(&mut self) -> Result<String, String> {
        self.eat(b'"')?;
        let mut out = String::new();
        loop {
            let Some(b) = self.peek() else {
                return Err(self.err("a closing quote"));
            };
            self.at += 1;
            match b {
                b'"' => return Ok(out),
                b'\\' => {
                    let Some(esc) = self.peek() else {
                        return Err(self.err("an escape"));
                    };
                    self.at += 1;
                    match esc {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let hi = self.hex4()?;
                            let ch = if (0xD800..0xDC00).contains(&hi) {
                                // A surrogate pair: `\uD83D\uDE00`.
                                if !self.src[self.at..].starts_with(b"\\u") {
                                    return Err(self.err("the low half of a surrogate pair"));
                                }
                                self.at += 2;
                                let lo = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&lo) {
                                    return Err(self.err("a low surrogate"));
                                }
                                0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                            } else {
                                hi
                            };
                            out.push(char::from_u32(ch).ok_or_else(|| self.err("a scalar value"))?);
                        }
                        _ => return Err(self.err("a JSON escape")),
                    }
                }
                b if b < 0x20 => return Err(self.err("no control byte inside a string")),
                _ => {
                    // Copy one whole UTF-8 sequence: the source is a `&str`, so
                    // the sequence starting here is well-formed.
                    let start = self.at - 1;
                    let len = utf8_len(b);
                    let end = (start + len).min(self.src.len());
                    out.push_str(
                        std::str::from_utf8(&self.src[start..end])
                            .map_err(|_| self.err("UTF-8"))?,
                    );
                    self.at = end;
                }
            }
        }
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let end = self.at + 4;
        let Some(slice) = self.src.get(self.at..end) else {
            return Err(self.err("four hex digits"));
        };
        let text = std::str::from_utf8(slice).map_err(|_| self.err("four hex digits"))?;
        let v = u32::from_str_radix(text, 16).map_err(|_| self.err("four hex digits"))?;
        self.at = end;
        Ok(v)
    }

    fn array(&mut self, depth: usize) -> Result<Json, String> {
        self.eat(b'[')?;
        let mut items = Vec::new();
        self.ws();
        if self.peek() == Some(b']') {
            self.at += 1;
            return Ok(Json::Array(items));
        }
        loop {
            self.ws();
            items.push(self.value(depth + 1)?);
            self.ws();
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(b']') => {
                    self.at += 1;
                    return Ok(Json::Array(items));
                }
                _ => return Err(self.err("`,` or `]`")),
            }
        }
    }

    fn object(&mut self, depth: usize) -> Result<Json, String> {
        self.eat(b'{')?;
        let mut members = Vec::new();
        self.ws();
        if self.peek() == Some(b'}') {
            self.at += 1;
            return Ok(Json::Object(members));
        }
        loop {
            self.ws();
            let key = self.string()?;
            self.ws();
            self.eat(b':')?;
            self.ws();
            let value = self.value(depth + 1)?;
            members.push((key, value));
            self.ws();
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(b'}') => {
                    self.at += 1;
                    return Ok(Json::Object(members));
                }
                _ => return Err(self.err("`,` or `}`")),
            }
        }
    }
}

/// The length of the UTF-8 sequence that starts with `lead`.
fn utf8_len(lead: u8) -> usize {
    match lead {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A vendor settings document survives a parse and a render with every
    /// key in its place and every number as written.
    #[test]
    fn a_settings_document_round_trips_in_order() {
        let text = r#"{
  "permissions": {
    "allow": ["Bash(git status)", "Read"],
    "deny": []
  },
  "env": {"FOO": "bar"},
  "hooks": {
    "Stop": [{"hooks": [{"type": "command", "command": "/x hook run stop", "timeout": 600}]}]
  },
  "ratio": 1.50,
  "big": 1e3,
  "neg": -0,
  "flag": true,
  "nothing": null
}"#;
        let doc = Json::parse(text).expect("parses");
        let keys: Vec<&str> = doc
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, _)| k.as_str())
            .collect();
        assert_eq!(
            keys,
            [
                "permissions",
                "env",
                "hooks",
                "ratio",
                "big",
                "neg",
                "flag",
                "nothing"
            ]
        );
        assert_eq!(doc.get("ratio"), Some(&Json::Number("1.50".into())));
        assert_eq!(doc.get("big"), Some(&Json::Number("1e3".into())));
        assert_eq!(doc.get("neg"), Some(&Json::Number("-0".into())));
        let again = Json::parse(&doc.render()).expect("the rendering parses");
        assert_eq!(again, doc);
        assert!(doc.render().contains("\"ratio\": 1.50"));
    }

    /// Every escape the grammar names, in both directions, including a
    /// surrogate pair and a raw multi-byte character.
    #[test]
    fn strings_escape_and_unescape() {
        let doc = Json::parse(r#""a\"b\\c\/d\b\f\n\r\t\u00e9\ud83d\ude00 héllo""#).unwrap();
        assert_eq!(
            doc,
            Json::Str("a\"b\\c/d\u{8}\u{c}\n\r\t\u{e9}\u{1F600} héllo".into())
        );
        assert_eq!(string("a\"b\\c\n\u{1b}"), "\"a\\\"b\\\\c\\n\\u001b\"");
        assert_eq!(string("héllo"), "\"héllo\"");
        let round = Json::parse(&Json::Str("tab\there \u{1F600}".into()).render()).unwrap();
        assert_eq!(round, Json::Str("tab\there \u{1F600}".into()));
    }

    /// Malformed input is refused with an offset — never accepted in part.
    #[test]
    fn malformed_documents_are_refused() {
        for bad in [
            "",
            "{",
            "[1,]",
            "{\"a\":}",
            "{\"a\" 1}",
            "01",
            "1.",
            "-",
            "1e",
            "\"unterminated",
            "\"bad\\escape\"",
            "\"ctrl\u{1}\"",
            "\"\\ud83d\"",
            "tru",
            "{} trailing",
            "\"\\u12\"",
        ] {
            assert!(Json::parse(bad).is_err(), "{bad:?} must be refused");
        }
        let deep = format!("{}1{}", "[".repeat(100), "]".repeat(100));
        assert!(Json::parse(&deep).is_err(), "nesting is bounded");
        let fine = format!("{}1{}", "[".repeat(20), "]".repeat(20));
        assert!(Json::parse(&fine).is_ok());
    }

    /// `entry` inserts once and answers the same member afterwards; `get` on
    /// a duplicate key answers the first, as the printer keeps both.
    #[test]
    fn entry_and_get() {
        let mut doc = Json::parse("{\"a\": 1, \"a\": 2}").unwrap();
        assert_eq!(doc.get("a"), Some(&Json::Number("1".into())));
        assert_eq!(
            doc.entry("b", Json::Array(vec![])),
            Some(&mut Json::Array(vec![]))
        );
        doc.entry("b", Json::Null)
            .unwrap()
            .as_array_mut()
            .unwrap()
            .push(Json::Null);
        assert_eq!(doc.get("b"), Some(&Json::Array(vec![Json::Null])));
        assert_eq!(doc.as_object().unwrap().len(), 3);
        assert!(Json::Null.entry("x", Json::Null).is_none());
        assert_eq!(
            doc.render(),
            "{\n  \"a\": 1,\n  \"a\": 2,\n  \"b\": [\n    null\n  ]\n}"
        );
        assert_eq!(Json::Object(vec![]).render(), "{}");
    }
}
