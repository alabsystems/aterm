// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Reader and writer for the XML property-list subset used by `defaults export` and `defaults write`
//! (`dict`, `array`, `string`, `integer`, `real`, `true`/`false`, `data`, and `date`).
//! [`document`] formats full plists matching `defaults export`, and [`fragment`] formats compact
//! single-line values for `defaults write`.

use crate::model::PValue;

const MAX_DEPTH: usize = 256;

const HEADER: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n";

/// Parse a whole XML plist document (`<?xml …?><plist><…/></plist>`) to its root value.
/// A bare value with no `<plist>` wrapper (a [`fragment`]) is read the same way.
///
/// # Errors
/// One line saying where the document stops being a plist.
pub fn parse(xml: &[u8]) -> Result<PValue, String> {
    Parser::new(xml, false).document(|p, tag| p.value_from(tag, 0))
}

/// Domain keys and values parsed by [`parse_domain`].
pub type Entries = Vec<(String, Result<PValue, String>)>;

/// Parse `defaults export <domain> -` output (whose root must be a dictionary) key by key,
/// recording out-of-range integers as per-key `Err` entries.
///
/// # Errors
/// One line saying where the document stops being a plist, or that its root is not a
/// dictionary.
pub fn parse_domain(xml: &[u8]) -> Result<Entries, String> {
    Parser::new(xml, true).document(Parser::domain)
}

/// Format `value` as a single-line XML fragment without a header (`<dict>…</dict>`).
#[must_use]
pub fn fragment(value: &PValue) -> String {
    let mut out = String::new();
    write_compact(&mut out, value);
    out
}

/// `value` as a whole XML plist document, laid out as `defaults export` lays it out.
#[must_use]
pub fn document(value: &PValue) -> String {
    let mut out = String::from(HEADER);
    write_pretty(&mut out, value, 0);
    out.push_str("</plist>\n");
    out
}

/// Format `r` as CoreFoundation writes `<real>` (`%.17g`, `nan`, `+infinity`, `-infinity`).
#[must_use]
pub fn real_text(r: f64) -> String {
    if r.is_nan() {
        return "nan".into();
    }
    if r.is_infinite() {
        return if r > 0.0 { "+infinity" } else { "-infinity" }.into();
    }
    let sci = format!("{r:.16e}");
    let (mantissa, exp) = sci.split_once('e').unwrap_or((sci.as_str(), "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    if (-4..17).contains(&exp) {
        let decimals = usize::try_from(16 - exp).unwrap_or(0);
        strip_fraction_zeros(&format!("{r:.decimals$}"))
    } else {
        let sign = if exp < 0 { '-' } else { '+' };
        format!(
            "{}e{sign}{:02}",
            strip_fraction_zeros(mantissa),
            exp.unsigned_abs()
        )
    }
}

fn strip_fraction_zeros(text: &str) -> String {
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_owned()
    } else {
        text.to_owned()
    }
}

fn push_escaped(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
}

fn base64(bytes: &[u8]) -> String {
    aterm_codec::base64::encode(bytes).unwrap_or_default()
}

fn write_scalar(out: &mut String, value: &PValue) -> bool {
    match value {
        PValue::Bool(true) => out.push_str("<true/>"),
        PValue::Bool(false) => out.push_str("<false/>"),
        PValue::Int(i) => {
            out.push_str("<integer>");
            out.push_str(&i.to_string());
            out.push_str("</integer>");
        }
        PValue::Real(r) => {
            out.push_str("<real>");
            out.push_str(&real_text(*r));
            out.push_str("</real>");
        }
        PValue::Str(s) => {
            out.push_str("<string>");
            push_escaped(out, s);
            out.push_str("</string>");
        }
        PValue::Date(d) => {
            out.push_str("<date>");
            push_escaped(out, d);
            out.push_str("</date>");
        }
        PValue::Data(_) | PValue::Array(_) | PValue::Dict(_) => return false,
    }
    true
}

fn write_compact(out: &mut String, value: &PValue) {
    if write_scalar(out, value) {
        return;
    }
    match value {
        PValue::Data(bytes) => {
            out.push_str("<data>");
            out.push_str(&base64(bytes));
            out.push_str("</data>");
        }
        PValue::Array(items) if items.is_empty() => out.push_str("<array/>"),
        PValue::Array(items) => {
            out.push_str("<array>");
            for item in items {
                write_compact(out, item);
            }
            out.push_str("</array>");
        }
        PValue::Dict(entries) if entries.is_empty() => out.push_str("<dict/>"),
        PValue::Dict(entries) => {
            out.push_str("<dict>");
            for (k, v) in entries {
                out.push_str("<key>");
                push_escaped(out, k);
                out.push_str("</key>");
                write_compact(out, v);
            }
            out.push_str("</dict>");
        }
        _ => {}
    }
}

fn indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push('\t');
    }
}

fn write_pretty(out: &mut String, value: &PValue, depth: usize) {
    indent(out, depth);
    if write_scalar(out, value) {
        out.push('\n');
        return;
    }
    match value {
        PValue::Data(bytes) => {
            out.push_str("<data>\n");
            let width = 76 - 8 * depth.min(8);
            let text = base64(bytes);
            for line in text.as_bytes().chunks(width) {
                indent(out, depth);
                out.push_str(&String::from_utf8_lossy(line));
                out.push('\n');
            }
            indent(out, depth);
            out.push_str("</data>\n");
        }
        PValue::Array(items) if items.is_empty() => out.push_str("<array/>\n"),
        PValue::Array(items) => {
            out.push_str("<array>\n");
            for item in items {
                write_pretty(out, item, depth + 1);
            }
            indent(out, depth);
            out.push_str("</array>\n");
        }
        PValue::Dict(entries) if entries.is_empty() => out.push_str("<dict/>\n"),
        PValue::Dict(entries) => {
            out.push_str("<dict>\n");
            for (k, v) in entries {
                indent(out, depth + 1);
                out.push_str("<key>");
                push_escaped(out, k);
                out.push_str("</key>\n");
                write_pretty(out, v, depth + 1);
            }
            indent(out, depth);
            out.push_str("</dict>\n");
        }
        _ => {}
    }
}

/// One tag, as the reader met it.
#[derive(Debug, PartialEq, Eq)]
enum Tag {
    /// `<name …>`.
    Open(String),
    /// `</name>`.
    Close(String),
    /// `<name …/>`.
    Empty(String),
}

impl std::fmt::Display for Tag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Open(n) => write!(f, "<{n}>"),
            Self::Close(n) => write!(f, "</{n}>"),
            Self::Empty(n) => write!(f, "<{n}/>"),
        }
    }
}

struct Parser<'a> {
    s: &'a [u8],
    pos: usize,
    /// Record out-of-range values in `unheld` instead of failing the document.
    lenient: bool,
    /// Why the value being read cannot be held, once a part of it could not.
    unheld: Option<String>,
}

impl<'a> Parser<'a> {
    const fn new(s: &'a [u8], lenient: bool) -> Self {
        Self {
            s,
            pos: 0,
            lenient,
            unheld: None,
        }
    }

    fn document<T>(
        &mut self,
        root: impl FnOnce(&mut Self, Tag) -> Result<T, String>,
    ) -> Result<T, String> {
        self.skip_prolog()?;
        let value = match self.tag()? {
            Tag::Open(name) if name == "plist" => {
                self.skip_misc()?;
                let tag = self.tag()?;
                let value = root(self, tag)?;
                self.skip_misc()?;
                match self.tag()? {
                    Tag::Close(name) if name == "plist" => {}
                    other => return Err(self.err(&format!("expected </plist>, found {other}"))),
                }
                value
            }
            Tag::Empty(name) if name == "plist" => {
                return Err(self.err("the document is an empty <plist/>"));
            }
            tag => root(self, tag)?,
        };
        self.skip_misc()?;
        if self.pos < self.s.len() {
            return Err(self.err("text after the root value"));
        }
        Ok(value)
    }

    fn domain(&mut self, tag: Tag) -> Result<Entries, String> {
        match tag {
            Tag::Empty(name) if name == "dict" => Ok(Vec::new()),
            Tag::Open(name) if name == "dict" => {
                let mut entries: Entries = Vec::new();
                loop {
                    self.unheld = None;
                    let Some((key, value)) = self.entry(0)? else {
                        return Ok(entries);
                    };
                    let value = match self.unheld.take() {
                        Some(why) => Err(why),
                        None => Ok(value),
                    };
                    upsert(&mut entries, key, value);
                }
            }
            other => Err(self.err(&format!("the root is {other}, not a dictionary"))),
        }
    }

    fn err(&self, what: &str) -> String {
        format!("plist: at byte {}: {what}", self.pos)
    }

    fn rest(&self) -> &[u8] {
        self.s.get(self.pos..).unwrap_or_default()
    }

    fn starts_with(&self, prefix: &[u8]) -> bool {
        self.rest().starts_with(prefix)
    }

    fn skip_past(&mut self, end: &[u8], what: &str) -> Result<(), String> {
        match find(self.rest(), end) {
            Some(at) => {
                self.pos += at + end.len();
                Ok(())
            }
            None => Err(self.err(&format!("unterminated {what}"))),
        }
    }

    fn skip_prolog(&mut self) -> Result<(), String> {
        let mut seen_doctype = false;
        loop {
            while self
                .s
                .get(self.pos)
                .is_some_and(|b| b.is_ascii_whitespace())
            {
                self.pos += 1;
            }
            if self.starts_with(b"<!--") {
                self.skip_past(b"-->", "comment")?;
            } else if self.starts_with(b"<?") {
                self.skip_past(b"?>", "processing instruction")?;
            } else if self.starts_with(b"<!DOCTYPE") {
                if !self
                    .s
                    .get(self.pos + b"<!DOCTYPE".len())
                    .is_some_and(|b| b.is_ascii_whitespace())
                {
                    return Err(self.err("malformed DOCTYPE"));
                }
                if seen_doctype {
                    return Err(self.err("duplicate DOCTYPE"));
                }
                seen_doctype = true;
                self.skip_doctype()?;
            } else {
                return Ok(());
            }
        }
    }

    fn skip_doctype(&mut self) -> Result<(), String> {
        self.pos += b"<!DOCTYPE".len();
        let mut quote: Option<u8> = None;
        loop {
            let Some(&b) = self.s.get(self.pos) else {
                return Err(self.err("unterminated DOCTYPE"));
            };
            match (quote, b) {
                (Some(q), b) if b == q => {
                    self.pos += 1;
                    quote = None;
                }
                (Some(_), _) => {
                    self.pos += 1;
                }
                (None, b'"' | b'\'') => {
                    self.pos += 1;
                    quote = Some(b);
                }
                (None, b'<') if self.starts_with(b"<!--") => {
                    self.skip_past(b"-->", "comment")?;
                }
                (None, b'<') => {
                    return Err(self.err("unexpected '<' in DOCTYPE"));
                }
                (None, b'[') => {
                    return Err(self.err("DOCTYPE internal subset is not supported"));
                }
                (None, b'>') => {
                    self.pos += 1;
                    return Ok(());
                }
                (None, _) => {
                    self.pos += 1;
                }
            }
        }
    }

    fn skip_misc(&mut self) -> Result<(), String> {
        loop {
            while self
                .s
                .get(self.pos)
                .is_some_and(|b| b.is_ascii_whitespace())
            {
                self.pos += 1;
            }
            if self.starts_with(b"<!--") {
                self.skip_past(b"-->", "comment")?;
            } else if self.starts_with(b"<?") {
                self.skip_past(b"?>", "processing instruction")?;
            } else {
                return Ok(());
            }
        }
    }

    fn tag(&mut self) -> Result<Tag, String> {
        if self.s.get(self.pos) != Some(&b'<') {
            return Err(if self.pos >= self.s.len() {
                self.err("the document ends where a tag should be")
            } else {
                self.err("expected a tag")
            });
        }
        self.pos += 1;
        let closing = self.s.get(self.pos) == Some(&b'/');
        if closing {
            self.pos += 1;
        }
        let start = self.pos;
        while self
            .s
            .get(self.pos)
            .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':'))
        {
            self.pos += 1;
        }
        let name =
            String::from_utf8_lossy(self.s.get(start..self.pos).unwrap_or_default()).into_owned();
        if name.is_empty() {
            return Err(self.err("a tag with no name"));
        }
        let mut quote: Option<u8> = None;
        loop {
            let Some(&b) = self.s.get(self.pos) else {
                return Err(self.err(&format!("unterminated tag <{name}")));
            };
            self.pos += 1;
            match (quote, b) {
                (Some(q), b) if b == q => quote = None,
                (Some(_), _) => {}
                (None, b'"' | b'\'') => quote = Some(b),
                (None, b'>') => {
                    return Ok(if closing {
                        Tag::Close(name)
                    } else {
                        Tag::Open(name)
                    });
                }
                (None, b'/') if self.s.get(self.pos) == Some(&b'>') && !closing => {
                    self.pos += 1;
                    return Ok(Tag::Empty(name));
                }
                (None, _) => {}
            }
        }
    }

    fn text(&mut self, name: &str) -> Result<String, String> {
        let mut bytes: Vec<u8> = Vec::new();
        loop {
            let Some(&b) = self.s.get(self.pos) else {
                return Err(self.err(&format!("unterminated <{name}>")));
            };
            match b {
                b'<' if self.starts_with(b"<![CDATA[") => {
                    let start = self.pos + 9;
                    self.skip_past(b"]]>", "CDATA section")?;
                    bytes.extend_from_slice(self.s.get(start..self.pos - 3).unwrap_or_default());
                }
                b'<' if self.starts_with(b"<!--") => self.skip_past(b"-->", "comment")?,
                b'<' => {
                    let at = self.pos;
                    match self.tag()? {
                        Tag::Close(n) if n == name => break,
                        other => {
                            self.pos = at;
                            return Err(self.err(&format!("{other} inside <{name}>")));
                        }
                    }
                }
                b'&' => {
                    let c = self.entity()?;
                    let mut buf = [0_u8; 4];
                    bytes.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                }
                b => {
                    bytes.push(b);
                    self.pos += 1;
                }
            }
        }
        String::from_utf8(bytes).map_err(|_| self.err(&format!("<{name}> is not UTF-8")))
    }

    fn entity(&mut self) -> Result<char, String> {
        let rest = self.rest();
        let Some(end) = rest.iter().take(12).position(|&b| b == b';') else {
            return Err(self.err("a bare '&' (an unterminated entity)"));
        };
        let body = String::from_utf8_lossy(rest.get(1..end).unwrap_or_default()).into_owned();
        let c = match body.as_str() {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            numeric => {
                let code = if let Some(hex) = numeric
                    .strip_prefix("#x")
                    .or_else(|| numeric.strip_prefix("#X"))
                {
                    (!hex.starts_with('+'))
                        .then(|| u32::from_str_radix(hex, 16).ok())
                        .flatten()
                } else {
                    numeric.strip_prefix('#').and_then(crate::parse_uint::<u32>)
                };
                code.and_then(char::from_u32)
            }
        };
        let Some(c) = c else {
            return Err(self.err(&format!("unknown entity &{body};")));
        };
        self.pos += end + 1;
        Ok(c)
    }

    fn value_from(&mut self, tag: Tag, depth: usize) -> Result<PValue, String> {
        if depth > MAX_DEPTH {
            return Err(self.err("containers nest too deeply"));
        }
        match tag {
            Tag::Empty(name) => match name.as_str() {
                "dict" => Ok(PValue::Dict(Vec::new())),
                "array" => Ok(PValue::Array(Vec::new())),
                "string" => Ok(PValue::Str(String::new())),
                "data" => Ok(PValue::Data(Vec::new())),
                "true" => Ok(PValue::Bool(true)),
                "false" => Ok(PValue::Bool(false)),
                other => Err(self.err(&format!("an empty <{other}/> is not a value"))),
            },
            Tag::Close(name) => Err(self.err(&format!("</{name}> where a value should be"))),
            Tag::Open(name) => match name.as_str() {
                "dict" => self.dict(depth),
                "array" => self.array(depth),
                "string" => Ok(PValue::Str(self.text("string")?)),
                "date" => Ok(PValue::Date(self.text("date")?.trim().to_owned())),
                "true" | "false" => {
                    let text = self.text(&name)?;
                    if !text.trim().is_empty() {
                        return Err(self.err(&format!("<{name}> holds text")));
                    }
                    Ok(PValue::Bool(name == "true"))
                }
                "integer" => {
                    let text = self.text("integer")?;
                    let text = text.trim();
                    match parse_integer(text) {
                        Some(i) => Ok(PValue::Int(i)),
                        None if self.lenient && integer_syntax(text) => {
                            self.unheld.get_or_insert_with(|| {
                                format!("<integer>{text}</integer> is past the 64-bit signed range this crate holds")
                            });
                            Ok(PValue::Int(0))
                        }
                        None => Err(self.err(&format!("<integer>{text}</integer>"))),
                    }
                }
                "real" => {
                    let text = self.text("real")?;
                    parse_real(text.trim())
                        .map(PValue::Real)
                        .ok_or_else(|| self.err(&format!("<real>{}</real>", text.trim())))
                }
                "data" => {
                    let text = self.text("data")?;
                    let compact: String =
                        text.chars().filter(|c| !c.is_ascii_whitespace()).collect();
                    aterm_codec::base64::decode(&compact)
                        .map(PValue::Data)
                        .map_err(|e| self.err(&format!("<data>: {e}")))
                }
                other => Err(self.err(&format!("<{other}> is not a plist value"))),
            },
        }
    }

    fn entry(&mut self, depth: usize) -> Result<Option<(String, PValue)>, String> {
        self.skip_misc()?;
        let key = match self.tag()? {
            Tag::Close(n) if n == "dict" => return Ok(None),
            Tag::Open(n) if n == "key" => self.text("key")?,
            Tag::Empty(n) if n == "key" => String::new(),
            other => return Err(self.err(&format!("{other} where a <key> should be"))),
        };
        self.skip_misc()?;
        let tag = self.tag()?;
        let value = self.value_from(tag, depth + 1)?;
        Ok(Some((key, value)))
    }

    fn dict(&mut self, depth: usize) -> Result<PValue, String> {
        let mut entries: Vec<(String, PValue)> = Vec::new();
        while let Some((key, value)) = self.entry(depth)? {
            upsert(&mut entries, key, value);
        }
        Ok(PValue::Dict(entries))
    }

    fn array(&mut self, depth: usize) -> Result<PValue, String> {
        let mut items = Vec::new();
        loop {
            self.skip_misc()?;
            match self.tag()? {
                Tag::Close(n) if n == "array" => return Ok(PValue::Array(items)),
                tag => items.push(self.value_from(tag, depth + 1)?),
            }
        }
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn upsert<V>(entries: &mut Vec<(String, V)>, key: String, value: V) {
    if let Some(slot) = entries.iter_mut().find(|(k, _)| *k == key) {
        slot.1 = value;
    } else {
        entries.push((key, value));
    }
}

fn integer_syntax(text: &str) -> bool {
    let digits = text.strip_prefix(['-', '+']).unwrap_or(text);
    match digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
    {
        Some(hex) => !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit()),
        None => !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()),
    }
}

fn parse_integer(text: &str) -> Option<i64> {
    // from_str_radix accepts its own sign; only the one outer sign belongs here.
    if !integer_syntax(text) {
        return None;
    }
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    let magnitude: i128 = match digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
    {
        Some(hex) if !hex.is_empty() => i128::from_str_radix(hex, 16).ok()?,
        Some(_) => return None,
        None if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) => {
            digits.parse::<i128>().ok()?
        }
        None => return None,
    };
    let signed = if negative {
        magnitude.checked_neg()?
    } else {
        magnitude
    };
    i64::try_from(signed).ok()
}

fn parse_real(text: &str) -> Option<f64> {
    match text.to_ascii_lowercase().as_str() {
        "nan" => Some(f64::NAN),
        "inf" | "+inf" | "infinity" | "+infinity" => Some(f64::INFINITY),
        "-inf" | "-infinity" => Some(f64::NEG_INFINITY),
        _ => text.parse::<f64>().ok(),
    }
}

#[cfg(test)]
#[path = "plist_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "plist_malformed_integer_boundary_tests.rs"]
mod malformed_integer_boundary_tests;
