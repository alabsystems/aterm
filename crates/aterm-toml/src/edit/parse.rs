// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The TOML 1.0.0 parser.

use core::ops::Range;

use super::item::{ArrayOfTables, Item, KeyMap, Table};
use super::value::{Array, InlineTable, Value, parsed};
use super::{Decor, DocumentMut, Key, PathRepr, RawString, Repr};
use crate::datetime::parse_datetime;
use crate::{Error, Result};

/// Bounds applied to untrusted input.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ParseLimits {
    /// Maximum depth of the tree below the root.
    pub(crate) max_depth: usize,
}

impl Default for ParseLimits {
    fn default() -> Self {
        Self { max_depth: 128 }
    }
}

/// A parsed key segment, with the source offsets the encoder needs to
/// reconstruct the line verbatim.
struct KeySeg {
    key: Key,
    start: usize,
    end: usize,
}

struct Parser<'a> {
    src: &'a str,
    b: &'a [u8],
    pos: usize,
    limits: ParseLimits,
    /// Monotonic counter over authored headers; a table's copy of it is what
    /// makes `[a.b]` written before `[a]` print back in that order.
    position: usize,
    /// Monotonic counter over authored key-value lines, stamped on each leaf
    /// key so flattening a dotted tree can restore the written order.
    key_order: usize,
}

pub(crate) fn parse_document(src: &str, limits: ParseLimits) -> Result<DocumentMut> {
    let mut p = Parser {
        src,
        b: src.as_bytes(),
        pos: 0,
        limits,
        position: 0,
        key_order: 0,
    };
    p.document()
}

/// Parse a standalone value (`Value::from_str`).
pub(crate) fn parse_value(src: &str) -> Result<Value> {
    let mut p = Parser {
        src,
        b: src.as_bytes(),
        pos: 0,
        limits: ParseLimits::default(),
        position: 0,
        key_order: 0,
    };
    let value = p.value(0)?;
    if p.pos != p.b.len() {
        return Err(Error::at(
            src,
            p.pos..p.b.len(),
            "trailing characters after value",
        ));
    }
    Ok(value)
}

/// Parse a dotted key expression on its own (`Key::parse`).
pub(crate) fn parse_key_path(repr: &str) -> Result<Vec<Key>> {
    let mut p = Parser {
        src: repr,
        b: repr.as_bytes(),
        pos: 0,
        limits: ParseLimits::default(),
        position: 0,
        key_order: 0,
    };
    p.eat_ws();
    let segs = p.key_path()?;
    if p.pos != p.b.len() {
        return Err(Error::at(
            repr,
            p.pos..p.b.len(),
            "trailing characters after key",
        ));
    }
    Ok(segs.into_iter().map(|s| s.key).collect())
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u8> {
        self.b.get(self.pos).copied()
    }

    fn at(&self, offset: usize) -> Option<u8> {
        self.b.get(self.pos + offset).copied()
    }

    fn eof(&self) -> bool {
        self.pos >= self.b.len()
    }

    fn err<T>(&self, span: Range<usize>, message: impl Into<String>) -> Result<T> {
        Err(Error::at(self.src, span, message))
    }

    /// A one-character span at the current position.
    fn here<T>(&self, message: impl Into<String>) -> Result<T> {
        self.err(self.pos..self.next_boundary(self.pos), message)
    }

    /// The first char boundary strictly after `at`, or the end of input.
    fn next_boundary(&self, at: usize) -> usize {
        let mut end = (at + 1).min(self.b.len());
        while end < self.b.len() && !self.src.is_char_boundary(end) {
            end += 1;
        }
        end.max(at)
    }

    /// Refuse a tree deeper than the limit.
    fn check_depth(&self, depth: usize, at: usize) -> Result<()> {
        if depth > self.limits.max_depth {
            return self.err(
                at..self.next_boundary(at),
                format!("nesting exceeds the {} level limit", self.limits.max_depth),
            );
        }
        Ok(())
    }

    /// Spaces and tabs only.
    fn eat_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t')) {
            self.pos += 1;
        }
    }

    /// A `#` comment, up to but not including the line terminator.
    fn eat_comment(&mut self) -> Result<()> {
        debug_assert_eq!(self.peek(), Some(b'#'));
        self.pos += 1;
        while let Some(c) = self.peek() {
            if c == b'\n' || c == b'\r' {
                break;
            }
            if c == b'\t' || (0x20..=0x7e).contains(&c) || c >= 0x80 {
                self.pos += 1;
            } else {
                return self.here(format!(
                    "control character U+{c:04X} is not allowed in a comment"
                ));
            }
        }
        Ok(())
    }

    fn eat_newline(&mut self) -> Result<bool> {
        match self.peek() {
            Some(b'\n') => {
                self.pos += 1;
                Ok(true)
            }
            Some(b'\r') => {
                if self.at(1) == Some(b'\n') {
                    self.pos += 2;
                    Ok(true)
                } else {
                    self.here("a carriage return must be followed by a line feed")
                }
            }
            _ => Ok(false),
        }
    }

    /// Whitespace, newlines, and comments — the trivia between top-level
    /// entries and inside arrays.
    fn eat_trivia(&mut self) -> Result<()> {
        loop {
            self.eat_ws();
            match self.peek() {
                Some(b'#') => self.eat_comment()?,
                Some(b'\n' | b'\r') => {
                    self.eat_newline()?;
                }
                _ => return Ok(()),
            }
        }
    }

    /// Everything from the end of a value or header to the start of the next
    /// line: whitespace, an optional comment, and the terminator itself, which
    /// belongs to the suffix (see the `edit` module docs).
    fn eat_line_end(&mut self) -> Result<()> {
        self.eat_ws();
        if self.peek() == Some(b'#') {
            self.eat_comment()?;
        }
        if self.eof() {
            return Ok(());
        }
        if self.eat_newline()? {
            return Ok(());
        }
        self.here("expected a newline after this value")
    }

    fn document(&mut self) -> Result<DocumentMut> {
        let mut doc = DocumentMut::new();
        let mut path: Vec<usize> = Vec::new();
        if self.src.starts_with('\u{feff}') {
            self.pos = 3;
            doc.bom = true;
        }
        let mut trivia_start = self.pos;
        let mut after_a_line = false;
        loop {
            self.eat_trivia()?;
            let at_header = self.peek() == Some(b'[');
            if after_a_line && (at_header || self.eof()) {
                let split = self.trailing_comment_end(trivia_start, self.pos);
                if split > trivia_start {
                    current_table(doc.as_item_mut(), &path).trailing =
                        RawString::from(self.src[trivia_start..split].to_owned());
                    trivia_start = split;
                }
            }
            if self.eof() {
                doc.trailing = RawString::from(self.src[trivia_start..self.pos].to_owned());
                return Ok(doc);
            }
            if at_header {
                path = self.table_header(&mut doc, trivia_start)?;
            } else {
                self.keyval(&mut doc, &path, trivia_start)?;
            }
            after_a_line = true;
            trivia_start = self.pos;
        }
    }

    /// Where the comment lines that touch the line before `start` end in the
    /// trivia run `start..end` (whole lines of comments and blanks): at the
    /// first blank line.
    fn trailing_comment_end(&self, start: usize, end: usize) -> usize {
        let run = &self.src[start..end];
        let mut at = start;
        for line in run.split_inclusive('\n') {
            if line.trim().is_empty() {
                return at;
            }
            at += line.len();
        }
        if end == self.src.len() { end } else { start }
    }

    fn table_header(&mut self, doc: &mut DocumentMut, trivia_start: usize) -> Result<Vec<usize>> {
        let header_start = self.pos;
        let array = self.at(1) == Some(b'[');
        self.pos += if array { 2 } else { 1 };
        self.eat_ws();
        let segs = self.key_path()?;
        self.check_depth(segs.len(), header_start)?;
        if self.peek() != Some(b']') {
            return self.here("expected `]` to close the table header");
        }
        self.pos += 1;
        if array {
            if self.peek() != Some(b']') {
                return self.here("expected `]]` to close the array-of-tables header");
            }
            self.pos += 1;
        }
        let header_end = self.pos;
        let inner = self.src
            [header_start + if array { 2 } else { 1 }..header_end - if array { 2 } else { 1 }]
            .to_owned();
        let suffix_start = self.pos;
        self.eat_line_end()?;
        let decor = Decor::new(
            self.src[trivia_start..header_start].to_owned(),
            self.src[suffix_start..self.pos].to_owned(),
        );
        self.open_table(
            doc,
            segs,
            array,
            decor,
            RawString::from(inner),
            header_start..header_end,
        )
    }

    fn open_table(
        &mut self,
        doc: &mut DocumentMut,
        segs: Vec<KeySeg>,
        array: bool,
        decor: Decor,
        header_repr: RawString,
        span: Range<usize>,
    ) -> Result<Vec<usize>> {
        let mut decor = Some(decor);
        let mut header_repr = Some(header_repr);
        let mut span = Some(span);
        let last = segs.len() - 1;
        let mut positions: Vec<usize> = Vec::with_capacity(segs.len());
        let mut node: &mut Table = doc.as_table_mut();

        for (i, seg) in segs.into_iter().enumerate() {
            let name = seg.key.get().to_owned();
            if i < last {
                if !node.contains_key(&name) {
                    let mut fresh = Table::new();
                    fresh.implicit = true;
                    node.insert_formatted(seg.key, Item::Table(fresh));
                }
                let position = node.items.position(&name).expect("just ensured present");
                positions.push(position);
                let existing = node
                    .items
                    .get_at_mut(position)
                    .expect("a position the map just returned");
                node = match existing {
                    Item::Table(t) => t,
                    Item::ArrayOfTables(a) => a
                        .values
                        .last_mut()
                        .expect("an array of tables always has an element"),
                    other => {
                        let what = other.type_name();
                        return self.err(
                            span.unwrap_or(0..0),
                            format!("cannot use `{name}` as a table: it is already {what}"),
                        );
                    }
                };
                continue;
            }

            let position = self.position;
            self.position += 1;
            if array {
                let mut fresh = Table::new();
                fresh.decor = decor.take().expect("a header is opened once");
                fresh.header_repr = header_repr.take();
                fresh.span = span.take();
                fresh.position = Some(position);
                match node.get_mut(&name) {
                    None => {
                        let mut aot = ArrayOfTables::new();
                        aot.push(fresh);
                        node.insert_formatted(seg.key, Item::ArrayOfTables(aot));
                    }
                    Some(Item::ArrayOfTables(a)) => a.push(fresh),
                    Some(other) => {
                        let what = other.type_name();
                        let at = fresh.span.clone().unwrap_or(0..0);
                        return self.err(
                            at,
                            format!("cannot append to `{name}` with `[[…]]`: it is already {what}"),
                        );
                    }
                }
            } else {
                match node.get_mut(&name) {
                    None => {
                        let mut fresh = Table::new();
                        fresh.decor = decor.take().expect("a header is opened once");
                        fresh.header_repr = header_repr.take();
                        fresh.span = span.clone();
                        fresh.position = Some(position);
                        node.insert_formatted(seg.key, Item::Table(fresh));
                    }
                    Some(Item::Table(t)) if t.implicit && !t.closed => {
                        t.implicit = false;
                        t.decor = decor.take().expect("a header is opened once");
                        t.header_repr = header_repr.take();
                        t.span = span.clone();
                        t.position = Some(position);
                    }
                    Some(other) => {
                        let what = if other.is_table() {
                            "a table"
                        } else {
                            other.type_name()
                        };
                        return self.err(
                            span.unwrap_or(0..0),
                            format!("`{name}` is defined twice: it is already {what}"),
                        );
                    }
                }
            }
            positions.push(
                node.items
                    .position(&name)
                    .expect("the header's table was just placed"),
            );
        }
        Ok(positions)
    }

    fn keyval(&mut self, doc: &mut DocumentMut, path: &[usize], trivia_start: usize) -> Result<()> {
        let key_start = self.pos;
        let segs = self.key_path()?;
        self.check_depth(path.len() + segs.len(), key_start)?;
        if self.peek() != Some(b'=') {
            return self.here("expected `=` after a key");
        }
        let leaf = segs.last().expect("a key path has at least one segment");
        let head = self.src[trivia_start..leaf.start].to_owned();
        let tail = self.src[leaf.end..self.pos].to_owned();
        self.pos += 1;

        let value_prefix_start = self.pos;
        self.eat_ws();
        let value_prefix = self.src[value_prefix_start..self.pos].to_owned();
        let mut value = self.value(path.len() + segs.len())?;
        let value_suffix_start = self.pos;
        self.eat_line_end()?;
        let decor = value.decor_mut();
        decor.set_prefix(value_prefix);
        decor.set_suffix(self.src[value_suffix_start..self.pos].to_owned());

        let order = self.key_order;
        self.key_order += 1;
        let table = current_table(doc.as_item_mut(), path);
        insert_path(
            self.src,
            &mut table.items,
            &segs,
            PathRepr {
                head: head.into(),
                tail: tail.into(),
            },
            order,
            Item::Value(value),
            false,
        )
    }

    /// One or more dot-separated key segments, consuming the whitespace around
    /// the dots.
    fn key_path(&mut self) -> Result<Vec<KeySeg>> {
        let mut segs = Vec::new();
        loop {
            self.eat_ws();
            let start = self.pos;
            let key = self.key_segment()?;
            let end = self.pos;
            segs.push(KeySeg { key, start, end });
            self.eat_ws();
            if self.peek() == Some(b'.') {
                self.pos += 1;
                continue;
            }
            return Ok(segs);
        }
    }

    fn key_segment(&mut self) -> Result<Key> {
        let start = self.pos;
        match self.peek() {
            Some(b'"') => {
                let text = self.basic_string(false)?;
                Ok(Key::with_repr(
                    text,
                    Repr::new_unchecked(self.src[start..self.pos].to_owned()),
                    start..self.pos,
                ))
            }
            Some(b'\'') => {
                let text = self.literal_string(false)?;
                Ok(Key::with_repr(
                    text,
                    Repr::new_unchecked(self.src[start..self.pos].to_owned()),
                    start..self.pos,
                ))
            }
            Some(c) if is_bare_key_byte(c) => {
                while matches!(self.peek(), Some(c) if is_bare_key_byte(c)) {
                    self.pos += 1;
                }
                let text = self.src[start..self.pos].to_owned();
                Ok(Key::with_repr(
                    text.clone(),
                    Repr::new_unchecked(text),
                    start..self.pos,
                ))
            }
            _ => self.here("expected a key"),
        }
    }

    /// Parse the value that fills a slot `depth` levels below the document
    /// root (see [`ParseLimits::max_depth`]).
    fn value(&mut self, depth: usize) -> Result<Value> {
        if depth > self.limits.max_depth {
            return self.here(format!(
                "value nesting exceeds the {} level limit",
                self.limits.max_depth
            ));
        }
        let start = self.pos;
        match self.peek() {
            Some(b'"') => {
                let text = self.basic_string(true)?;
                Ok(Value::String(parsed(
                    text,
                    &self.src[start..self.pos],
                    start..self.pos,
                )))
            }
            Some(b'\'') => {
                let text = self.literal_string(true)?;
                Ok(Value::String(parsed(
                    text,
                    &self.src[start..self.pos],
                    start..self.pos,
                )))
            }
            Some(b'[') => self.array(depth).map(Value::Array),
            Some(b'{') => self.inline_table(depth).map(Value::InlineTable),
            Some(b't') if self.src[self.pos..].starts_with("true") => {
                self.pos += 4;
                self.end_of_scalar(start)?;
                Ok(Value::Boolean(parsed(true, "true", start..self.pos)))
            }
            Some(b'f') if self.src[self.pos..].starts_with("false") => {
                self.pos += 5;
                self.end_of_scalar(start)?;
                Ok(Value::Boolean(parsed(false, "false", start..self.pos)))
            }
            Some(_) => self.number_or_datetime(),
            None => self.here("expected a value"),
        }
    }

    /// After a bare scalar the next byte has to end it.
    fn end_of_scalar(&mut self, start: usize) -> Result<()> {
        match self.peek() {
            None | Some(b' ' | b'\t' | b'\n' | b'\r' | b',' | b']' | b'}' | b'#') => Ok(()),
            Some(_) => {
                // The whole offending character: callers slice the source with the
                let end = self.next_boundary(self.pos);
                self.err(start..end, "unexpected characters after a value")
            }
        }
    }

    fn number_or_datetime(&mut self) -> Result<Value> {
        let start = self.pos;
        if let Some((dt, used)) = parse_datetime(&self.b[self.pos..]) {
            self.pos += used;
            if self.end_of_scalar(start).is_ok() {
                let raw = self.src[start..self.pos].to_owned();
                return Ok(Value::Datetime(parsed(dt, &raw, start..self.pos)));
            }
            self.pos = start;
        }
        while let Some(c) = self.peek() {
            if matches!(c, b' ' | b'\t' | b'\n' | b'\r' | b',' | b']' | b'}' | b'#') {
                break;
            }
            self.pos += 1;
        }
        let raw = &self.src[start..self.pos];
        if raw.is_empty() {
            return self.here("expected a value");
        }
        match parse_number(raw) {
            Some(Number::Integer(v)) => Ok(Value::Integer(parsed(v, raw, start..self.pos))),
            Some(Number::Float(v)) => Ok(Value::Float(parsed(v, raw, start..self.pos))),
            None => self.err(
                start..self.pos,
                format!("`{raw}` is not a valid TOML value"),
            ),
        }
    }

    fn array(&mut self, depth: usize) -> Result<Array> {
        let start = self.pos;
        self.pos += 1;
        let mut array = Array::new();
        loop {
            let trivia_start = self.pos;
            self.eat_trivia()?;
            if self.peek() == Some(b']') {
                array.trailing = RawString::from(self.src[trivia_start..self.pos].to_owned());
                self.pos += 1;
                array.span = Some(start..self.pos);
                return Ok(array);
            }
            if self.eof() {
                // byte, where a whole-array span would underline the opening bracket.
                return self.err(self.b.len()..self.b.len(), "unterminated array");
            }
            let prefix = self.src[trivia_start..self.pos].to_owned();
            let mut value = self.value(depth + 1)?;
            let suffix_start = self.pos;
            self.eat_trivia()?;
            let decor = value.decor_mut();
            decor.set_prefix(prefix);
            decor.set_suffix(self.src[suffix_start..self.pos].to_owned());
            array.values.push(value);
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                    array.trailing_comma = true;
                }
                Some(b']') => {
                    array.trailing_comma = false;
                    array.trailing = RawString::default();
                    self.pos += 1;
                    array.span = Some(start..self.pos);
                    return Ok(array);
                }
                _ => return self.here("expected `,` or `]` in an array"),
            }
        }
    }

    fn inline_table(&mut self, depth: usize) -> Result<InlineTable> {
        let start = self.pos;
        self.pos += 1;
        let mut table = InlineTable::new();
        let preamble_start = self.pos;
        self.eat_ws();
        if self.peek() == Some(b'}') {
            table.preamble = RawString::from(self.src[preamble_start..self.pos].to_owned());
            self.pos += 1;
            table.span = Some(start..self.pos);
            return Ok(table);
        }
        self.pos = preamble_start;

        loop {
            let entry_start = self.pos;
            let segs = self.key_path()?;
            self.check_depth(depth + segs.len(), entry_start)?;
            if self.peek() != Some(b'=') {
                return self.here("expected `=` after a key in an inline table");
            }
            let leaf = segs.last().expect("a key path has at least one segment");
            let head = self.src[entry_start..leaf.start].to_owned();
            let tail = self.src[leaf.end..self.pos].to_owned();
            self.pos += 1;

            let value_prefix_start = self.pos;
            self.eat_ws();
            let value_prefix = self.src[value_prefix_start..self.pos].to_owned();
            let mut value = self.value(depth + segs.len())?;
            let value_suffix_start = self.pos;
            self.eat_ws();
            let decor = value.decor_mut();
            decor.set_prefix(value_prefix);
            decor.set_suffix(self.src[value_suffix_start..self.pos].to_owned());

            let order = self.key_order;
            self.key_order += 1;
            insert_path(
                self.src,
                &mut table.items,
                &segs,
                PathRepr {
                    head: head.into(),
                    tail: tail.into(),
                },
                order,
                Item::Value(value),
                true,
            )?;

            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    table.span = Some(start..self.pos);
                    return Ok(table);
                }
                Some(b'\n' | b'\r') => {
                    return self.here(
                        "a TOML 1.0 inline table must fit on one line: newlines are not allowed",
                    );
                }
                _ => return self.here("expected `,` or `}` in an inline table"),
            }
        }
    }

    /// A `"…"` or `"""…"""` string.
    fn basic_string(&mut self, multiline_ok: bool) -> Result<String> {
        let start = self.pos;
        if multiline_ok && self.at(1) == Some(b'"') && self.at(2) == Some(b'"') {
            return self.multiline_string(start, b'"');
        }
        self.pos += 1;
        let mut out = String::new();
        loop {
            let Some(c) = self.peek() else {
                return self.err(start..self.b.len(), "unterminated string");
            };
            match c {
                b'"' => {
                    self.pos += 1;
                    return Ok(out);
                }
                b'\\' => self.escape(&mut out, false)?,
                b'\n' | b'\r' => {
                    return self.here("a single-line string cannot contain a newline");
                }
                _ => self.string_char(&mut out, 0x7e)?,
            }
        }
    }

    /// A `'…'` or `'''…'''` string.
    fn literal_string(&mut self, multiline_ok: bool) -> Result<String> {
        let start = self.pos;
        if multiline_ok && self.at(1) == Some(b'\'') && self.at(2) == Some(b'\'') {
            return self.multiline_string(start, b'\'');
        }
        self.pos += 1;
        let mut out = String::new();
        loop {
            let Some(c) = self.peek() else {
                return self.err(start..self.b.len(), "unterminated literal string");
            };
            match c {
                b'\'' => {
                    self.pos += 1;
                    return Ok(out);
                }
                b'\n' | b'\r' => {
                    return self.here("a single-line literal string cannot contain a newline");
                }
                _ => self.string_char(&mut out, 0x7e)?,
            }
        }
    }

    /// The `"""`/`'''` forms.
    fn multiline_string(&mut self, start: usize, quote: u8) -> Result<String> {
        self.pos += 3;
        if self.peek() == Some(b'\r') && self.at(1) == Some(b'\n') {
            self.pos += 2;
        } else if self.peek() == Some(b'\n') {
            self.pos += 1;
        }
        let mut out = String::new();
        loop {
            let Some(c) = self.peek() else {
                return self.err(start..self.b.len(), "unterminated multi-line string");
            };
            if c == quote {
                let mut run = 0;
                while self.at(run) == Some(quote) {
                    run += 1;
                }
                if run >= 3 {
                    if run > 5 {
                        return self.here(
                            "too many quotes: a multi-line string may hold at most two before its \
                             closing delimiter",
                        );
                    }
                    for _ in 0..run - 3 {
                        out.push(char::from(quote));
                    }
                    self.pos += run;
                    return Ok(out);
                }
                for _ in 0..run {
                    out.push(char::from(quote));
                }
                self.pos += run;
                continue;
            }
            match c {
                b'\\' if quote == b'"' => self.escape(&mut out, true)?,
                b'\r' => {
                    if self.at(1) == Some(b'\n') {
                        out.push('\n');
                        self.pos += 2;
                    } else {
                        return self.here("a carriage return must be followed by a line feed");
                    }
                }
                b'\n' => {
                    out.push('\n');
                    self.pos += 1;
                }
                _ => self.string_char(&mut out, 0x7e)?,
            }
        }
    }

    /// Copy one source character into `out`, rejecting the control bytes the
    /// spec forbids unescaped.
    fn string_char(&mut self, out: &mut String, max_ascii: u8) -> Result<()> {
        let c = self.b[self.pos];
        if c < 0x80 {
            if c != b'\t' && (c < 0x20 || c > max_ascii) {
                return self.here(format!(
                    "control character U+{c:04X} must be escaped inside a string"
                ));
            }
            out.push(char::from(c));
            self.pos += 1;
            return Ok(());
        }
        let ch = self.src[self.pos..]
            .chars()
            .next()
            .expect("source is valid UTF-8");
        out.push(ch);
        self.pos += ch.len_utf8();
        Ok(())
    }

    fn escape(&mut self, out: &mut String, multiline: bool) -> Result<()> {
        let start = self.pos;
        self.pos += 1;
        let Some(c) = self.peek() else {
            return self.err(start..self.b.len(), "unterminated escape sequence");
        };
        self.pos += 1;
        match c {
            b'b' => out.push('\u{8}'),
            b't' => out.push('\t'),
            b'n' => out.push('\n'),
            b'f' => out.push('\u{c}'),
            b'r' => out.push('\r'),
            b'"' => out.push('"'),
            b'\\' => out.push('\\'),
            b'u' => self.unicode_escape(out, 4, start)?,
            b'U' => self.unicode_escape(out, 8, start)?,
            b' ' | b'\t' | b'\n' | b'\r' if multiline => {
                self.pos = start + 1;
                self.eat_ws();
                if !self.eat_newline()? {
                    return self.err(
                        start..self.pos,
                        "a `\\` must be followed by an escape or a newline",
                    );
                }
                loop {
                    self.eat_ws();
                    if !self.eat_newline()? {
                        break;
                    }
                }
            }
            _ => {
                // The offending character, not byte: `c` may lead a multi-byte one.
                let ch = self.src[start + 1..]
                    .chars()
                    .next()
                    .expect("a byte was peeked here, so a character starts here");
                return self.err(
                    start..self.next_boundary(start + 1),
                    format!("`\\{ch}` is not a TOML escape sequence"),
                );
            }
        }
        Ok(())
    }

    fn unicode_escape(&mut self, out: &mut String, digits: usize, start: usize) -> Result<()> {
        if self.pos + digits > self.b.len() {
            return self.err(start..self.b.len(), "truncated unicode escape");
        }
        let window = &self.b[self.pos..self.pos + digits];
        if !window.iter().all(u8::is_ascii_hexdigit) {
            return self.err(
                start..self.next_boundary(self.pos),
                "unicode escape needs hexadecimal digits",
            );
        }
        let code = window.iter().fold(0u32, |acc, &c| {
            acc * 16 + char::from(c).to_digit(16).unwrap_or(0)
        });
        let Some(ch) = char::from_u32(code) else {
            return self.err(
                start..self.pos + digits,
                format!("U+{code:04X} is not a Unicode scalar value"),
            );
        };
        self.pos += digits;
        out.push(ch);
        Ok(())
    }
}

fn is_bare_key_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'-'
}

enum Number {
    Integer(i64),
    Float(f64),
}

/// Classify and decode a bare numeric token, enforcing the spec's underscore,
/// leading-zero, and sign rules.
fn parse_number(raw: &str) -> Option<Number> {
    match raw {
        "inf" | "+inf" => return Some(Number::Float(f64::INFINITY)),
        "-inf" => return Some(Number::Float(f64::NEG_INFINITY)),
        "nan" | "+nan" => return Some(Number::Float(f64::NAN)),
        "-nan" => return Some(Number::Float(-f64::NAN)),
        _ => {}
    }

    let (sign, rest) = match raw.as_bytes().first() {
        Some(b'+') => (1i64, &raw[1..]),
        Some(b'-') => (-1i64, &raw[1..]),
        _ => (1i64, raw),
    };
    if rest.is_empty() {
        return None;
    }

    if sign == 1 && !raw.starts_with('+') && rest.len() > 2 && rest.starts_with('0') {
        let radix = match rest.as_bytes()[1] {
            b'x' => Some(16),
            b'o' => Some(8),
            b'b' => Some(2),
            _ => None,
        };
        if let Some(radix) = radix {
            let digits = strip_underscores(&rest[2..], radix)?;
            return i64::from_str_radix(&digits, radix)
                .ok()
                .map(Number::Integer);
        }
    }

    let is_float = rest.contains('.')
        || rest.contains('e')
        || rest.contains('E')
        || rest.contains("inf")
        || rest.contains("nan");

    if !is_float {
        let digits = strip_underscores(rest, 10)?;
        if digits.len() > 1 && digits.starts_with('0') {
            return None;
        }
        let signed = if sign < 0 {
            format!("-{digits}")
        } else {
            digits
        };
        return signed.parse::<i64>().ok().map(Number::Integer);
    }

    let normalized = validate_float(rest)?;
    let value = normalized.parse::<f64>().ok()?;
    if value.is_infinite() {
        return None;
    }
    Some(Number::Float(if sign < 0 { -value } else { value }))
}

/// Remove underscores, refusing any that is not flanked by digits of `radix`.
fn strip_underscores(text: &str, radix: u32) -> Option<String> {
    let bytes = text.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let mut out = String::with_capacity(bytes.len());
    for (i, &c) in bytes.iter().enumerate() {
        if c == b'_' {
            let before = i
                .checked_sub(1)
                .map(|j| bytes[j])
                .filter(|b| char::from(*b).is_digit(radix));
            let after = bytes
                .get(i + 1)
                .copied()
                .filter(|b| char::from(*b).is_digit(radix));
            if before.is_none() || after.is_none() {
                return None;
            }
            continue;
        }
        if !char::from(c).is_digit(radix) {
            return None;
        }
        out.push(char::from(c));
    }
    Some(out)
}

/// Check an unsigned float body against `float-int-part ( exp / frac [ exp ] )`
/// and return it with the underscores removed, ready for `str::parse`.
fn validate_float(text: &str) -> Option<String> {
    let (mantissa, exponent) = match text.find(['e', 'E']) {
        Some(i) => (&text[..i], Some(&text[i + 1..])),
        None => (text, None),
    };
    let (int_part, frac_part) = match mantissa.find('.') {
        Some(i) => (&mantissa[..i], Some(&mantissa[i + 1..])),
        None => (mantissa, None),
    };
    if exponent.is_none() && frac_part.is_none() {
        return None;
    }

    let int_digits = strip_underscores(int_part, 10)?;
    if int_digits.len() > 1 && int_digits.starts_with('0') {
        return None;
    }
    let mut out = int_digits;

    if let Some(frac) = frac_part {
        let frac_digits = strip_underscores(frac, 10)?;
        out.push('.');
        out.push_str(&frac_digits);
    }

    if let Some(exp) = exponent {
        let (exp_sign, exp_body) = match exp.as_bytes().first() {
            Some(b'+') => ("", &exp[1..]),
            Some(b'-') => ("-", &exp[1..]),
            _ => ("", exp),
        };
        let exp_digits = strip_underscores(exp_body, 10)?;
        out.push('e');
        out.push_str(exp_sign);
        out.push_str(&exp_digits);
    }

    Some(out)
}

/// Resolve the table a key-value line belongs to, following the header path
/// built by [`Parser::open_table`].
fn current_table<'t>(root: &'t mut Item, path: &[usize]) -> &'t mut Table {
    let mut node = root
        .as_table_mut()
        .expect("document root is always a table");
    for &position in path {
        node = match node
            .items
            .get_at_mut(position)
            .expect("header path was created when the header parsed")
        {
            Item::Table(t) => t,
            Item::ArrayOfTables(a) => a
                .values
                .last_mut()
                .expect("an array of tables always has an element"),
            _ => unreachable!("a header path never resolves to a value"),
        };
    }
    node
}

/// Write `value` at the dotted path `segs`, creating the intermediate dotted
/// tables and enforcing the spec's duplicate rules.
fn insert_path(
    src: &str,
    map: &mut KeyMap,
    segs: &[KeySeg],
    path_repr: PathRepr,
    order: usize,
    value: Item,
    inline: bool,
) -> Result<()> {
    let last = segs.len() - 1;
    let mut node: &mut KeyMap = map;
    for seg in &segs[..last] {
        let name = seg.key.get().to_owned();
        if !node.contains_key(&name) {
            let mut key = seg.key.clone();
            key.decor_mut().clear();
            let item = if inline {
                let mut t = InlineTable::new();
                t.dotted = true;
                Item::Value(Value::InlineTable(t))
            } else {
                let mut t = Table::new();
                t.implicit = true;
                t.dotted = true;
                t.closed = true;
                Item::Table(t)
            };
            node.insert(key, item);
        }
        let entry = node.get_mut(&name).expect("just ensured present");
        let extendable = match &*entry {
            Item::Table(t) => t.dotted,
            Item::Value(Value::InlineTable(t)) => t.dotted,
            _ => false,
        };
        if !extendable {
            let what = entry.type_name();
            return Err(Error::at(
                src,
                seg.start..seg.end,
                format!("cannot extend `{name}` with a dotted key: it is already {what}"),
            ));
        }
        node = match entry {
            Item::Table(t) => &mut t.items,
            Item::Value(Value::InlineTable(t)) => &mut t.items,
            _ => unreachable!("just checked this entry is a dotted table"),
        };
    }

    let leaf = &segs[last];
    if node.contains_key(leaf.key.get()) {
        return Err(Error::at(
            src,
            leaf.start..leaf.end,
            format!("`{}` is defined twice in this table", leaf.key.get()),
        ));
    }
    let mut key = leaf.key.clone();
    key.path_repr = Some(path_repr);
    key.order = order;
    node.insert(key, value);
    Ok(())
}
