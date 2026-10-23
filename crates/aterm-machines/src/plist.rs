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
        self.skip_misc()?;
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
            } else if self.starts_with(b"<!DOCTYPE") {
                self.skip_past(b">", "DOCTYPE")?;
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
                    u32::from_str_radix(hex, 16).ok()
                } else {
                    numeric
                        .strip_prefix('#')
                        .and_then(|d| d.parse::<u32>().ok())
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
mod tests {
    use super::*;

    const VM_CURRENT_HOST_GLOBAL: &[u8] =
        include_bytes!("../fixtures/prefs/vm-27.0-currenthost-global.plist");

    const VM_EVERY_TYPE: &[u8] = include_bytes!("../fixtures/prefs/vm-27.0-every-type.plist");

    const VM_MISSING_DOMAIN: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict/>\n</plist>\n";

    fn get<'a>(dict: &'a PValue, key: &str) -> &'a PValue {
        let PValue::Dict(entries) = dict else {
            panic!("not a dict: {dict}");
        };
        &entries
            .iter()
            .find(|(k, _)| k == key)
            .unwrap_or_else(|| panic!("no key {key}"))
            .1
    }

    #[test]
    fn the_vm_current_host_global_domain_parses_with_its_caps_lock_mapping() {
        let root = parse(VM_CURRENT_HOST_GLOBAL).unwrap();
        let mapping = get(&root, "com.apple.keyboard.modifiermapping.1452-1-0");
        assert_eq!(
            mapping,
            &PValue::Array(vec![PValue::Dict(vec![
                (
                    "HIDKeyboardModifierMappingDst".into(),
                    PValue::Int(30_064_771_296)
                ),
                (
                    "HIDKeyboardModifierMappingSrc".into(),
                    PValue::Int(30_064_771_129)
                ),
            ])])
        );
        assert_eq!(
            get(&root, "com.apple.trackpad.enableSecondaryClick"),
            &PValue::Bool(true)
        );
        assert_eq!(
            get(&root, "com.apple.trackpad.threeFingerDragGesture"),
            &PValue::Bool(false)
        );
        assert_eq!(get(&root, "com.apple.trackpad.version"), &PValue::Int(5));
        assert_eq!(
            get(&root, "com.apple.gms.enhancedSiri.lastUpdated"),
            &PValue::Date("2026-10-16T22:40:34Z".into())
        );
        let PValue::Data(bytes) = get(&root, "com.apple.gms.availability.unifiedReasons") else {
            panic!("not data");
        };
        assert_eq!(
            String::from_utf8_lossy(bytes),
            r#"{"com.apple.Settings.AppleIntelligence.Partner.ChatGPT":[{"unavailable":{"_0":{"partnerNotSelected":{}}}}]}"#
        );
    }

    #[test]
    fn a_document_is_written_back_exactly_as_defaults_export_printed_it() {
        for fixture in [
            VM_CURRENT_HOST_GLOBAL,
            VM_EVERY_TYPE,
            VM_MISSING_DOMAIN.as_bytes(),
        ] {
            let value = parse(fixture).unwrap();
            assert_eq!(document(&value), String::from_utf8_lossy(fixture));
        }
    }

    #[test]
    fn a_missing_domain_is_an_empty_dictionary() {
        assert_eq!(
            parse(VM_MISSING_DOMAIN.as_bytes()).unwrap(),
            PValue::Dict(Vec::new())
        );
    }

    #[test]
    fn every_type_the_vm_wrote_reads_back_with_its_type() {
        let root = parse(VM_EVERY_TYPE).unwrap();
        assert_eq!(get(&root, "int"), &PValue::Int(5));
        assert_eq!(get(&root, "neg"), &PValue::Int(-30_064_771_129));
        assert_eq!(get(&root, "real"), &PValue::Real(1.5));
        assert_eq!(get(&root, "t"), &PValue::Bool(true));
        assert_eq!(get(&root, "f"), &PValue::Bool(false));
        assert_eq!(get(&root, "data"), &PValue::Data(vec![0, 1, 2, 0xff]));
        assert_eq!(
            get(&root, "date"),
            &PValue::Date("2026-10-16T22:40:34Z".into())
        );
        assert_eq!(get(&root, "emptystr"), &PValue::Str(String::new()));
        assert_eq!(get(&root, "emptyarr"), &PValue::Array(Vec::new()));
        assert_eq!(get(&root, "emptydict"), &PValue::Dict(Vec::new()));
        assert_eq!(
            get(&root, "str"),
            &PValue::Str("a & b <c> \"q\" 's' \u{e9} \u{263a}".into())
        );
        assert_eq!(
            get(&root, "dict"),
            &PValue::Dict(vec![
                (
                    "a".into(),
                    PValue::Dict(vec![("n".into(), PValue::Array(vec![PValue::Real(0.25)]))])
                ),
                ("b".into(), PValue::Int(2)),
            ])
        );
    }

    fn every_type() -> PValue {
        PValue::Dict(vec![
            ("b".into(), PValue::Bool(true)),
            ("i".into(), PValue::Int(i64::MIN)),
            ("r".into(), PValue::Real(0.1)),
            ("tiny".into(), PValue::Real(1e-7)),
            ("big".into(), PValue::Real(6.02e23)),
            ("s".into(), PValue::Str("<a> & 'b' \"c\" é\ttab".into())),
            ("empty".into(), PValue::Str(String::new())),
            ("d".into(), PValue::Data((0..=255).collect())),
            ("nodata".into(), PValue::Data(Vec::new())),
            ("t".into(), PValue::Date("2026-10-16T22:40:34Z".into())),
            (
                "a".into(),
                PValue::Array(vec![
                    PValue::Array(Vec::new()),
                    PValue::Dict(Vec::new()),
                    PValue::Dict(vec![("deep".into(), PValue::Data(vec![7; 100]))]),
                ]),
            ),
        ])
    }

    #[test]
    fn a_fragment_and_a_document_read_back_to_the_value_they_were_made_from() {
        let value = every_type();
        assert_eq!(parse(fragment(&value).as_bytes()).unwrap(), value);
        assert_eq!(parse(document(&value).as_bytes()).unwrap(), value);
        assert!(!fragment(&value).contains('\n'));
    }

    #[test]
    fn a_fragment_is_the_one_line_form_the_vm_accepted() {
        let mapping = PValue::Array(vec![PValue::Dict(vec![
            (
                "HIDKeyboardModifierMappingSrc".into(),
                PValue::Int(30_064_771_129),
            ),
            (
                "HIDKeyboardModifierMappingDst".into(),
                PValue::Int(30_064_771_296),
            ),
        ])]);
        assert_eq!(
            fragment(&mapping),
            "<array><dict><key>HIDKeyboardModifierMappingSrc</key><integer>30064771129</integer><key>HIDKeyboardModifierMappingDst</key><integer>30064771296</integer></dict></array>"
        );
        assert_eq!(
            fragment(&PValue::Data(vec![0, 1, 2, 0xff])),
            "<data>AAEC/w==</data>"
        );
    }

    #[test]
    fn reals_are_written_as_corefoundation_writes_them() {
        assert_eq!(real_text(0.1), "0.10000000000000001");
        assert_eq!(real_text(1e-7), "9.9999999999999995e-08");
        assert_eq!(real_text(30_064_771_129.5), "30064771129.5");
        assert_eq!(real_text(3.0), "3");
        assert_eq!(real_text(0.25), "0.25");
        assert_eq!(real_text(f64::from(0.1_f32)), "0.10000000149011612");
        assert_eq!(real_text(f64::INFINITY), "+infinity");
        assert_eq!(real_text(f64::NEG_INFINITY), "-infinity");
        assert_eq!(real_text(f64::NAN), "nan");
        assert_eq!(real_text(1e300), "1.0000000000000001e+300");
        for r in [0.1, 1e-7, 1e300, -2.5, 0.083_333_333, f64::MIN_POSITIVE] {
            assert_eq!(parse_real(&real_text(r)), Some(r));
        }
    }

    #[test]
    fn integers_read_in_decimal_or_hex_and_refuse_what_does_not_fit() {
        assert_eq!(parse_integer("30064771129"), Some(30_064_771_129));
        assert_eq!(parse_integer("-1"), Some(-1));
        assert_eq!(parse_integer("+7"), Some(7));
        assert_eq!(parse_integer("0x7000000E0"), Some(30_064_771_296));
        assert_eq!(parse_integer("18446744073709551615"), None);
        assert_eq!(parse_integer("1.5"), None);
        assert_eq!(parse_integer(""), None);
        assert_eq!(parse_integer("0x"), None);
    }

    #[test]
    fn a_broken_document_is_refused_with_where_it_broke() {
        for (text, needle) in [
            (
                "<plist><dict><key>a</key></dict></plist>",
                "where a value should be",
            ),
            (
                "<plist><dict><key>a</key><integer>x</integer></dict></plist>",
                "<integer>x",
            ),
            ("<plist><array><string>a</array></plist>", "inside <string>"),
            ("<plist><dict>", "ends where a tag"),
            (
                "<plist><string>a &bogus; b</string></plist>",
                "unknown entity",
            ),
            ("<plist><string>a & b</string></plist>", "bare '&'"),
            ("<plist><data>!!!!</data></plist>", "<data>"),
            ("<plist><integer/></plist>", "not a value"),
            ("<plist><true/></plist>trailing", "after the root"),
            ("<plist><blob/></plist>", "not a value"),
            ("<plist/>", "empty <plist/>"),
        ] {
            let err = parse(text.as_bytes()).unwrap_err();
            assert!(err.contains(needle), "{text}: {err}");
            assert!(err.starts_with("plist: at byte "), "{err}");
        }
    }

    #[test]
    fn cdata_comments_and_a_repeated_key_read_as_corefoundation_reads_them() {
        let text = "<!-- c --><plist version=\"1.0\"><dict><!-- x --><key>a</key><string><![CDATA[<raw> & ]]>tail</string><key>a</key><integer>2</integer></dict></plist>";
        assert_eq!(
            parse(text.as_bytes()).unwrap(),
            PValue::Dict(vec![("a".into(), PValue::Int(2))])
        );
        let text = "<string><![CDATA[<raw> & ]]>tail</string>";
        assert_eq!(
            parse(text.as_bytes()).unwrap(),
            PValue::Str("<raw> & tail".into())
        );
    }

    #[test]
    fn an_integer_past_the_signed_range_fails_only_its_own_key_of_a_domain() {
        let text = "<plist version=\"1.0\"><dict><key>apps</key><array><dict><key>flags</key><integer>9223372036854775815</integer></dict></array><key>k</key><true/><key>big</key><integer>13713454596331600536</integer></dict></plist>";
        let entries = parse_domain(text.as_bytes()).unwrap();
        let keys: Vec<&str> = entries.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["apps", "k", "big"]);
        assert_eq!(entries[1].1, Ok(PValue::Bool(true)));
        for (key, value) in [&entries[0], &entries[2]] {
            let why = value.clone().unwrap_err();
            assert!(why.contains("past the 64-bit signed range"), "{key}: {why}");
        }
        assert!(
            parse(text.as_bytes())
                .unwrap_err()
                .contains("<integer>9223372036854775815")
        );
        let broken = "<plist><dict><key>k</key><integer>12x</integer></dict></plist>";
        assert!(
            parse_domain(broken.as_bytes())
                .unwrap_err()
                .contains("<integer>12x")
        );
    }

    #[test]
    fn a_domain_whose_root_is_not_a_dictionary_is_refused() {
        assert_eq!(
            parse_domain(VM_MISSING_DOMAIN.as_bytes()).unwrap(),
            Vec::new()
        );
        let err = parse_domain(b"<plist><array/></plist>").unwrap_err();
        assert!(err.contains("not a dictionary"), "{err}");
        let entries = parse_domain(VM_EVERY_TYPE).unwrap();
        let PValue::Dict(whole) = parse(VM_EVERY_TYPE).unwrap() else {
            panic!("not a dict");
        };
        let held: Vec<(String, PValue)> =
            entries.into_iter().map(|(k, v)| (k, v.unwrap())).collect();
        assert_eq!(held, whole);
    }

    #[test]
    fn nesting_past_the_guard_is_refused_rather_than_overflowing_the_stack() {
        let deep = format!("{}{}", "<array>".repeat(10_000), "</array>".repeat(10_000));
        let err = parse(deep.as_bytes()).unwrap_err();
        assert!(err.contains("nest too deeply"), "{err}");
    }
}

#[cfg(test)]
mod malformed_integer_boundary_tests {
    use super::{parse, parse_domain};
    use crate::model::PValue;

    #[test]
    fn malformed_signed_hex_integers_are_refused_without_panicking() {
        for (text, wanted) in [
            ("0", 0),
            ("+42", 42),
            ("-128", -128),
            ("0x7f", 127),
            ("-0x80", -128),
            ("9223372036854775807", i64::MAX),
            ("-9223372036854775808", i64::MIN),
            ("-0x8000000000000000", i64::MIN),
        ] {
            let document = format!("<integer>{text}</integer>");
            assert_eq!(parse(document.as_bytes()).unwrap(), PValue::Int(wanted));
        }
        let partial = parse_domain(b"<dict><key>large</key><integer>9223372036854775808</integer><key>kept</key><integer>7</integer></dict>").unwrap();
        assert!(
            partial[0].1.is_err(),
            "a valid out-of-range integer remains an unheld domain value"
        );
        assert_eq!(partial[1].1, Ok(PValue::Int(7)));
        let refused: Vec<bool> = [
            "-0x-80000000000000000000000000000000",
            "0x-1",
            "-0x+1",
            "0x+1",
            "--1",
            "+",
        ]
        .into_iter()
        .map(|text| {
            let document = format!("<integer>{text}</integer>");
            std::panic::catch_unwind(|| parse(document.as_bytes()))
                .is_ok_and(|result| result.is_err())
        })
        .collect();
        assert_eq!(
            refused, [true; 6],
            "every malformed form is observed before the aggregate assertion"
        );
    }
}
