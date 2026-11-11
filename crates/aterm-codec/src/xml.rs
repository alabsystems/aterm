// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! XML's text escape, shared by every plist and XML document the workspace writes, and a
//! small pull [`Reader`] for the documents it reads (OOXML parts among them).

/// `text` with XML's five special characters escaped, for element text and attribute values alike.
#[must_use]
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    escape_into(&mut out, text);
    out
}

/// [`escape`] appending into a caller-owned buffer.
pub fn escape_into(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
}

/// The deepest element nesting [`Reader`] accepts.
pub const MAX_DEPTH: usize = 256;

/// Why a document was refused, and where: a fixed reason and a byte offset, never any
/// of the document's own text (a document may hold secrets).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error {
    reason: &'static str,
    offset: usize,
}

impl Error {
    /// The fixed, value-free reason.
    #[must_use]
    pub const fn reason(&self) -> &'static str {
        self.reason
    }

    /// The byte offset in the document where the refused construct starts.
    #[must_use]
    pub const fn offset(&self) -> usize {
        self.offset
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("xml: ")?;
        f.write_str(self.reason)?;
        f.write_str(" at byte ")?;
        crate::write_usize_decimal(f, self.offset)
    }
}

impl std::error::Error for Error {}

/// An element's start: its name (prefix kept, as in `x:row`) and attributes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag<'a> {
    /// The element name as written, prefix included.
    pub name: &'a str,
    /// Whether the tag closed itself (`<x/>`): no [`Event::End`] follows it.
    pub empty: bool,
    attrs: Vec<(&'a str, String)>,
}

impl<'a> Tag<'a> {
    /// The unescaped value of the attribute named exactly `name` (prefix included).
    #[must_use]
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| v.as_str())
    }

    /// Every attribute in document order, values unescaped.
    pub fn attrs(&self) -> impl Iterator<Item = (&'a str, &str)> {
        self.attrs.iter().map(|(n, v)| (*n, v.as_str()))
    }
}

/// One step of a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event<'a> {
    /// An element starts; a self-closing one has `empty` set and no matching end.
    Start(Tag<'a>),
    /// The element with this name ends.
    End(&'a str),
    /// Character data between tags, unescaped, whitespace included, or a CDATA section's
    /// contents verbatim.
    Text(String),
}

/// A pull reader over a whole document held as `&str`.
///
/// It skips the XML declaration, processing instructions, comments and a DOCTYPE without
/// an internal subset; it checks that every end tag matches the open element, that the
/// document closes every element, that nesting stays within [`MAX_DEPTH`] and that no
/// tag repeats an attribute. After an error the reader is spent and refuses again.
///
/// # Examples
///
/// ```rust
/// use aterm_codec::xml::{Event, Reader};
///
/// let mut r = Reader::new(r#"<?xml version="1.0"?><c r="A1" t='s'><v>1 &amp; 2</v><x/></c>"#);
/// let Ok(Some(Event::Start(c))) = r.next_event() else { panic!() };
/// assert_eq!((c.name, c.attr("r"), c.attr("t")), ("c", Some("A1"), Some("s")));
/// assert!(matches!(r.next_event(), Ok(Some(Event::Start(v))) if v.name == "v"));
/// assert_eq!(r.next_event(), Ok(Some(Event::Text("1 & 2".into()))));
/// assert_eq!(r.next_event(), Ok(Some(Event::End("v"))));
/// assert!(matches!(r.next_event(), Ok(Some(Event::Start(x))) if x.empty));
/// assert_eq!(r.next_event(), Ok(Some(Event::End("c"))));
/// assert_eq!(r.next_event(), Ok(None));
/// ```
#[derive(Debug, Clone)]
pub struct Reader<'a> {
    text: &'a str,
    pos: usize,
    open: Vec<&'a str>,
    failed: Option<Error>,
}

/// XML's whitespace: space, tab, carriage return, line feed.
const fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n')
}

/// A byte that ends a name: whitespace or one of XML's tag delimiters.
const fn ends_name(b: u8) -> bool {
    is_space(b)
        || matches!(
            b,
            b'/' | b'>' | b'=' | b'<' | b'"' | b'\'' | b'&' | b'?' | b'!'
        )
}

/// `raw` with entity and character references replaced; `base` is `raw`'s offset in the
/// document, for errors. A `<` is refused (text never holds one: the reader stops there).
fn unescape(raw: &str, base: usize) -> Result<String, Error> {
    let bad = |reason, at: usize| Error {
        reason,
        offset: base.saturating_add(at),
    };
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    let mut at = 0usize;
    while let Some(i) = rest.find(['&', '<']) {
        let (plain, tail) = rest.split_at(i);
        out.push_str(plain);
        at = at.saturating_add(i);
        if tail.starts_with('<') {
            return Err(bad("'<' in an attribute value", at));
        }
        let body_end = tail
            .get(1..)
            .and_then(|t| t.find(';'))
            .ok_or_else(|| bad("unterminated reference", at))?;
        let name = tail
            .get(1..body_end.saturating_add(1))
            .ok_or_else(|| bad("unterminated reference", at))?;
        let c = match name {
            "lt" => '<',
            "gt" => '>',
            "amp" => '&',
            "quot" => '"',
            "apos" => '\'',
            _ => {
                let num = name
                    .strip_prefix('#')
                    .ok_or_else(|| bad("unknown entity", at))?;
                let (digits, radix) = match num.strip_prefix('x') {
                    Some(hex) => (hex, 16),
                    None => (num, 10),
                };
                if digits.is_empty() || digits.len() > 8 {
                    return Err(bad("invalid character reference", at));
                }
                let code = u32::from_str_radix(digits, radix)
                    .map_err(|_| bad("invalid character reference", at))?;
                match char::from_u32(code) {
                    Some(c) if c != '\0' => c,
                    _ => return Err(bad("invalid character reference", at)),
                }
            }
        };
        out.push(c);
        // `&`, the name, `;`.
        let consumed = name.len().saturating_add(2);
        at = at.saturating_add(consumed);
        rest = tail.get(consumed..).unwrap_or("");
    }
    out.push_str(rest);
    Ok(out)
}

impl<'a> Reader<'a> {
    /// A reader at the start of `text`.
    #[must_use]
    pub const fn new(text: &'a str) -> Self {
        Self {
            text,
            pos: 0,
            open: Vec::new(),
            failed: None,
        }
    }

    /// The byte offset of the next unread construct.
    #[must_use]
    pub const fn offset(&self) -> usize {
        self.pos
    }

    /// The next event, or `None` at the end of a document that closed every element.
    ///
    /// # Errors
    ///
    /// A malformed document: an unterminated tag, comment, CDATA section or processing
    /// instruction; a DOCTYPE with an internal subset; a bad name, attribute or reference;
    /// an unknown entity; a repeated attribute; an end tag that does not match the open
    /// element; nesting past [`MAX_DEPTH`]; or input that ends inside an element.
    pub fn next_event(&mut self) -> Result<Option<Event<'a>>, Error> {
        if let Some(e) = self.failed {
            return Err(e);
        }
        let got = self.step();
        if let Err(e) = got {
            self.failed = Some(e);
        }
        got
    }

    fn err(&self, reason: &'static str) -> Error {
        Error {
            reason,
            offset: self.pos,
        }
    }

    fn rest(&self) -> &'a str {
        self.text.get(self.pos..).unwrap_or("")
    }

    /// Moves past the next `delim`, or refuses with `reason`.
    fn skip_past(&mut self, delim: &str, reason: &'static str) -> Result<(), Error> {
        let i = self.rest().find(delim).ok_or_else(|| self.err(reason))?;
        self.pos = self.pos.saturating_add(i).saturating_add(delim.len());
        Ok(())
    }

    fn skip_space(&mut self) {
        let n = self.rest().bytes().take_while(|&b| is_space(b)).count();
        self.pos = self.pos.saturating_add(n);
    }

    fn name(&mut self) -> Result<&'a str, Error> {
        let rest = self.rest();
        let n = rest.bytes().take_while(|&b| !ends_name(b)).count();
        let name = rest.get(..n).unwrap_or("");
        if name.is_empty() {
            return Err(self.err("expected a name"));
        }
        self.pos = self.pos.saturating_add(n);
        Ok(name)
    }

    fn step(&mut self) -> Result<Option<Event<'a>>, Error> {
        loop {
            let rest = self.rest();
            if rest.is_empty() {
                if self.open.is_empty() {
                    return Ok(None);
                }
                return Err(self.err("input ends inside an element"));
            }
            if !rest.starts_with('<') {
                let n = rest.find('<').unwrap_or(rest.len());
                let raw = rest.get(..n).unwrap_or("");
                let text = unescape(raw, self.pos)?;
                self.pos = self.pos.saturating_add(n);
                return Ok(Some(Event::Text(text)));
            }
            if rest.starts_with("<?") {
                self.skip_past("?>", "unterminated processing instruction")?;
            } else if rest.starts_with("<!--") {
                self.pos = self.pos.saturating_add(4);
                self.skip_past("-->", "unterminated comment")?;
            } else if let Some(body) = rest.strip_prefix("<![CDATA[") {
                let n = body
                    .find("]]>")
                    .ok_or_else(|| self.err("unterminated CDATA section"))?;
                let text = body.get(..n).unwrap_or("").to_owned();
                self.pos = self
                    .pos
                    .saturating_add(9)
                    .saturating_add(n)
                    .saturating_add(3);
                return Ok(Some(Event::Text(text)));
            } else if rest.starts_with("<!DOCTYPE") {
                self.doctype()?;
            } else if rest.starts_with("</") {
                return self.end_tag().map(Some);
            } else {
                return self.start_tag().map(Some);
            }
        }
    }

    /// Skips a DOCTYPE, honouring quoted literals; refuses an internal subset.
    fn doctype(&mut self) -> Result<(), Error> {
        let start = self.pos;
        let mut quote: Option<u8> = None;
        for (i, b) in self.rest().bytes().enumerate() {
            match (quote, b) {
                (Some(q), b) if b == q => quote = None,
                (Some(_), _) => {}
                (None, b'"' | b'\'') => quote = Some(b),
                (None, b'[') => {
                    self.pos = start.saturating_add(i);
                    return Err(self.err("a DOCTYPE internal subset is not supported"));
                }
                (None, b'>') => {
                    self.pos = start.saturating_add(i).saturating_add(1);
                    return Ok(());
                }
                (None, _) => {}
            }
        }
        Err(self.err("unterminated DOCTYPE"))
    }

    fn end_tag(&mut self) -> Result<Event<'a>, Error> {
        let start = self.pos;
        self.pos = self.pos.saturating_add(2);
        let name = self.name()?;
        self.skip_space();
        if !self.rest().starts_with('>') {
            return Err(self.err("malformed end tag"));
        }
        self.pos = self.pos.saturating_add(1);
        match self.open.pop() {
            Some(open) if open == name => Ok(Event::End(name)),
            _ => Err(Error {
                reason: "end tag does not match the open element",
                offset: start,
            }),
        }
    }

    fn start_tag(&mut self) -> Result<Event<'a>, Error> {
        let start = self.pos;
        self.pos = self.pos.saturating_add(1);
        let name = self.name()?;
        let mut attrs: Vec<(&'a str, String)> = Vec::new();
        loop {
            let before = self.pos;
            self.skip_space();
            let rest = self.rest();
            if rest.starts_with("/>") {
                self.pos = self.pos.saturating_add(2);
                return Ok(Event::Start(Tag {
                    name,
                    empty: true,
                    attrs,
                }));
            }
            if rest.starts_with('>') {
                self.pos = self.pos.saturating_add(1);
                if self.open.len() >= MAX_DEPTH {
                    return Err(Error {
                        reason: "elements nest too deeply",
                        offset: start,
                    });
                }
                self.open.push(name);
                return Ok(Event::Start(Tag {
                    name,
                    empty: false,
                    attrs,
                }));
            }
            if rest.is_empty() {
                return Err(self.err("unterminated tag"));
            }
            if self.pos == before {
                return Err(self.err("attributes need whitespace before them"));
            }
            let attr_at = self.pos;
            let key = self.name()?;
            self.skip_space();
            if !self.rest().starts_with('=') {
                return Err(self.err("attribute without a value"));
            }
            self.pos = self.pos.saturating_add(1);
            self.skip_space();
            let quote = match self.rest().bytes().next() {
                Some(q @ (b'"' | b'\'')) => q,
                _ => return Err(self.err("attribute value is not quoted")),
            };
            self.pos = self.pos.saturating_add(1);
            let rest = self.rest();
            let n = rest
                .bytes()
                .position(|b| b == quote)
                .ok_or_else(|| self.err("unterminated attribute value"))?;
            let value = unescape(rest.get(..n).unwrap_or(""), self.pos)?;
            self.pos = self.pos.saturating_add(n).saturating_add(1);
            if attrs.iter().any(|(k, _)| *k == key) {
                return Err(Error {
                    reason: "attribute repeated in one tag",
                    offset: attr_at,
                });
            }
            attrs.push((key, value));
        }
    }
}

#[cfg(test)]
#[path = "xml_tests.rs"]
mod tests;
