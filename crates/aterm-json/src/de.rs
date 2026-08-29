// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The parser and its `serde::Deserializer`.
//!
//! RFC 8259 with the same three practical rules `serde_json` applies, because
//! every one of them is load-bearing for a program that reads untrusted JSON:
//!
//! * **trailing data is an error.** `from_str` parses ONE value and then
//!   requires end-of-input, so `{"a":1} {"b":2}` is a failure rather than the
//!   first object.
//! * **nesting is bounded** ([`RECURSION_LIMIT`]). A hostile document of ten
//!   thousand `[` is an error, not a stack overflow — and the recursion here is
//!   the deserializer's own, so the bound has to be enforced by counting rather
//!   than hoped for.
//! * **duplicate keys are last-wins**, matching `serde_json` and what
//!   `#[derive(Deserialize)]` does with them. (This is the OPPOSITE of
//!   `aterm-toml`, which rejects them — a config file an operator edits and a
//!   wire format a server emits are different problems.)

use std::borrow::Cow;

use serde::de::{self, IntoDeserializer as _, Visitor};

use crate::error::{Error, Result};

/// Maximum nesting depth, matching `serde_json`'s default. Reached by arrays
/// and objects alike; the count is the deserializer's own recursion, which is
/// what a stack overflow would come from.
pub const RECURSION_LIMIT: u32 = 128;

/// A JSON deserializer over a byte slice.
pub struct Deserializer<'de> {
    input: &'de [u8],
    pos: usize,
    depth: u32,
    /// Refuse an integer literal too wide for `u64`/`i64` instead of widening
    /// it to `f64`.
    ///
    /// OFF for `from_str`/`from_slice`, which must read what a server sent the
    /// way `serde_json` reads it: a 30-digit integer in a wire document IS a
    /// float to both. ON for [`crate::to_value`], whose contract is a LOSSLESS
    /// round-trip through the text form — see the note there.
    exact_integers: bool,
}

impl<'de> Deserializer<'de> {
    /// Build a deserializer over `input`.
    #[must_use]
    pub fn from_slice(input: &'de [u8]) -> Self {
        Self {
            input,
            pos: 0,
            depth: 0,
            exact_integers: false,
        }
    }

    /// As [`Self::from_slice`], but an integer literal that does not fit
    /// `u64`/`i64` is an ERROR rather than an `f64`.
    pub(crate) fn exact(input: &'de [u8]) -> Self {
        Self {
            exact_integers: true,
            ..Self::from_slice(input)
        }
    }

    fn err(&self, message: impl Into<String>) -> Error {
        Error::at(self.input, self.pos, message)
    }

    fn eof(&self) -> Error {
        self.err("unexpected end of JSON input")
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }

    fn bump(&mut self) {
        self.pos = self.pos.saturating_add(1);
    }

    /// Skip RFC 8259 whitespace: space, tab, newline, carriage return. Nothing
    /// else — a JSON comment is not whitespace, and neither is a form feed.
    fn skip_ws(&mut self) {
        while let Some(byte) = self.peek() {
            match byte {
                b' ' | b'\t' | b'\n' | b'\r' => self.bump(),
                _ => break,
            }
        }
    }

    fn peek_value(&mut self) -> Result<u8> {
        self.skip_ws();
        self.peek().ok_or_else(|| self.eof())
    }

    fn expect(&mut self, want: u8, what: &str) -> Result<()> {
        self.skip_ws();
        match self.peek() {
            Some(byte) if byte == want => {
                self.bump();
                Ok(())
            }
            Some(_) => Err(self.err(format!("expected {what}"))),
            None => Err(self.eof()),
        }
    }

    fn literal(&mut self, word: &[u8], what: &str) -> Result<()> {
        let end = self.pos.saturating_add(word.len());
        if self.input.get(self.pos..end) == Some(word) {
            self.pos = end;
            Ok(())
        } else {
            Err(self.err(format!("expected {what}")))
        }
    }

    fn enter(&mut self) -> Result<()> {
        self.depth = self.depth.saturating_add(1);
        // The 128th container is already too deep — `serde_json` starts a
        // budget of `RECURSION_LIMIT` and refuses the descent that would spend
        // the last unit, so 127 nested arrays parse and 128 do not. The
        // differential oracle pinned this boundary; it was off by one.
        if self.depth >= RECURSION_LIMIT {
            return Err(self.err("recursion limit exceeded"));
        }
        Ok(())
    }

    fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    /// Everything after the parsed value must be whitespace.
    pub(crate) fn end(&mut self) -> Result<()> {
        self.skip_ws();
        if self.pos >= self.input.len() {
            Ok(())
        } else {
            Err(self.err("trailing characters after JSON value"))
        }
    }

    // ── strings ────────────────────────────────────────────────────────────

    /// Parse a string body, the opening quote already consumed.
    ///
    /// Returns a BORROWED slice whenever the body contains no escapes, which is
    /// the overwhelmingly common case and what keeps struct deserialization
    /// allocation-free for every field name.
    fn parse_string_body(&mut self) -> Result<Cow<'de, str>> {
        let start = self.pos;
        loop {
            let Some(byte) = self.peek() else {
                return Err(self.eof());
            };
            match byte {
                b'"' => {
                    let raw = self.input.get(start..self.pos).unwrap_or(&[]);
                    self.bump();
                    return match core::str::from_utf8(raw) {
                        Ok(text) => Ok(Cow::Borrowed(text)),
                        Err(_) => Err(self.err("invalid UTF-8 in string")),
                    };
                }
                b'\\' => break,
                // RFC 8259: unescaped control characters are not allowed.
                0x00..=0x1F => return Err(self.err("control character in string")),
                _ => self.bump(),
            }
        }
        // Escaped: build an owned string, starting from what was already scanned.
        let head = self.input.get(start..self.pos).unwrap_or(&[]);
        let mut out = match core::str::from_utf8(head) {
            Ok(text) => String::from(text),
            Err(_) => return Err(self.err("invalid UTF-8 in string")),
        };
        let mut chunk = self.pos;
        loop {
            let Some(byte) = self.peek() else {
                return Err(self.eof());
            };
            match byte {
                b'"' => {
                    self.push_chunk(&mut out, chunk)?;
                    self.bump();
                    return Ok(Cow::Owned(out));
                }
                b'\\' => {
                    self.push_chunk(&mut out, chunk)?;
                    self.bump();
                    self.parse_escape(&mut out)?;
                    chunk = self.pos;
                }
                0x00..=0x1F => return Err(self.err("control character in string")),
                _ => self.bump(),
            }
        }
    }

    fn push_chunk(&self, out: &mut String, from: usize) -> Result<()> {
        let raw = self.input.get(from..self.pos).unwrap_or(&[]);
        match core::str::from_utf8(raw) {
            Ok(text) => {
                out.push_str(text);
                Ok(())
            }
            Err(_) => Err(self.err("invalid UTF-8 in string")),
        }
    }

    /// Parse one escape sequence, the backslash already consumed.
    fn parse_escape(&mut self, out: &mut String) -> Result<()> {
        let Some(byte) = self.peek() else {
            return Err(self.eof());
        };
        self.bump();
        let simple = match byte {
            b'"' => Some('"'),
            b'\\' => Some('\\'),
            b'/' => Some('/'),
            b'b' => Some('\u{8}'),
            b'f' => Some('\u{c}'),
            b'n' => Some('\n'),
            b'r' => Some('\r'),
            b't' => Some('\t'),
            b'u' => None,
            _ => return Err(self.err("invalid escape sequence")),
        };
        if let Some(ch) = simple {
            out.push(ch);
            return Ok(());
        }
        let first = self.parse_hex4()?;
        // A surrogate half is only meaningful as part of a pair; a lone one is
        // not a character and cannot be encoded, so it is refused rather than
        // silently replaced.
        let ch = if (0xD800..0xDC00).contains(&first) {
            if self.peek() != Some(b'\\') {
                return Err(self.err("unpaired high surrogate in \\u escape"));
            }
            self.bump();
            if self.peek() != Some(b'u') {
                return Err(self.err("unpaired high surrogate in \\u escape"));
            }
            self.bump();
            let second = self.parse_hex4()?;
            if !(0xDC00..0xE000).contains(&second) {
                return Err(self.err("invalid low surrogate in \\u escape"));
            }
            // U+10000 + ((hi - D800) << 10) + (lo - DC00).
            let hi = u32::from(first).saturating_sub(0xD800);
            let lo = u32::from(second).saturating_sub(0xDC00);
            let combined = 0x1_0000u32
                .saturating_add(hi.saturating_mul(0x400))
                .saturating_add(lo);
            char::from_u32(combined).ok_or_else(|| self.err("invalid \\u escape"))?
        } else if (0xDC00..0xE000).contains(&first) {
            return Err(self.err("unpaired low surrogate in \\u escape"));
        } else {
            char::from_u32(u32::from(first)).ok_or_else(|| self.err("invalid \\u escape"))?
        };
        out.push(ch);
        Ok(())
    }

    fn parse_hex4(&mut self) -> Result<u16> {
        let mut value: u16 = 0;
        for _ in 0..4 {
            let Some(byte) = self.peek() else {
                return Err(self.eof());
            };
            let digit = match byte {
                b'0'..=b'9' => u16::from(byte.saturating_sub(b'0')),
                b'a'..=b'f' => u16::from(byte.saturating_sub(b'a')).saturating_add(10),
                b'A'..=b'F' => u16::from(byte.saturating_sub(b'A')).saturating_add(10),
                _ => return Err(self.err("invalid hex digit in \\u escape")),
            };
            // Four hex digits never exceed u16, so neither op saturates.
            value = value.wrapping_mul(16).wrapping_add(digit);
            self.bump();
        }
        Ok(value)
    }

    // ── numbers ────────────────────────────────────────────────────────────

    /// Validate a JSON number against the grammar and classify it.
    ///
    /// An integer that fits `u64`/`i64` stays an integer; anything else — a
    /// fraction, an exponent, or a magnitude past 64 bits — becomes `f64`,
    /// which is exactly where `serde_json` draws the line.
    ///
    /// The exact text and whether it carried a fraction or an exponent come
    /// back too: the 128-bit entry points need them to widen an integer the
    /// `f64` classification would have rounded.
    fn parse_number(&mut self) -> Result<Parsed<'de>> {
        let start = self.pos;
        let negative = self.peek() == Some(b'-');
        if negative {
            self.bump();
        }
        // int = "0" / digit1-9 *DIGIT — a leading zero may not be followed by
        // more digits.
        match self.peek() {
            Some(b'0') => {
                self.bump();
                if matches!(self.peek(), Some(b'0'..=b'9')) {
                    return Err(self.err("number has a leading zero"));
                }
            }
            Some(b'1'..=b'9') => {
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.bump();
                }
            }
            Some(_) => return Err(self.err("invalid number")),
            None => return Err(self.eof()),
        }
        let mut floating = false;
        if self.peek() == Some(b'.') {
            floating = true;
            self.bump();
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.err("number has no digits after the decimal point"));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.bump();
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            floating = true;
            self.bump();
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.bump();
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.err("number has no digits in its exponent"));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.bump();
            }
        }
        let raw = self.input.get(start..self.pos).unwrap_or(&[]);
        let Ok(text) = core::str::from_utf8(raw) else {
            return Err(self.err("invalid number"));
        };
        let wrap = |value| Parsed {
            value,
            text,
            floating,
        };
        if !floating {
            if negative {
                if let Ok(value) = text.parse::<i64>() {
                    // `-0` is not the integer zero to `serde_json`: with no
                    // fraction and no exponent it still parses as the FLOAT
                    // negative zero, so `Value` renders it `-0.0` AND a typed
                    // `u64` field REFUSES it. Kept as its own case so the
                    // f64 conversion below cannot lose the sign.
                    return Ok(wrap(if value == 0 {
                        Num::NegZero
                    } else {
                        Num::Signed(value)
                    }));
                }
            } else if let Ok(value) = text.parse::<u64>() {
                return Ok(wrap(Num::Unsigned(value)));
            }
            if self.exact_integers {
                // Strict mode: an integer literal past 64 bits would become an
                // f64 below, which is exactly the rounding `to_value` must not
                // do silently.
                return Err(self.err("number out of range"));
            }
        }
        // Out of integer range, or genuinely fractional. `f64::from_str` accepts
        // a superset of the JSON grammar, but the grammar was already enforced
        // above, so this only ever sees a valid number.
        match text.parse::<f64>() {
            // A magnitude past `f64::MAX` is refused rather than quietly
            // becoming infinity — JSON cannot represent infinity, so a document
            // that reads back as `null` would not be the document that was
            // parsed. Underflow to zero is NOT refused: `1e-400` is zero to
            // every JSON reader, `serde_json` included.
            Ok(value) if value.is_infinite() => Err(self.err("number out of range")),
            Ok(value) => Ok(wrap(Num::Float(value))),
            Err(_) => Err(self.err("invalid number")),
        }
    }

    /// Hand a parsed number to `visitor`.
    ///
    /// `-0` goes to the visitor as the FLOAT negative zero in every context,
    /// typed or not. That is what `serde_json` does and it is not an accident
    /// of its untyped path: its typed integer entry points call the SAME
    /// `parse_number`, which turns `-0` into `F64(-0.0)` on both paths, so
    /// `from_str::<u64>("-0")` is an error there. It used to be `Ok(0)` here —
    /// an acceptance-set widening on the reader that parses the GitHub Releases
    /// discovery reply.
    fn visit_number<V: Visitor<'de>>(&mut self, visitor: V) -> Result<V::Value> {
        let parsed = self.parse_number()?;
        Self::dispatch(parsed.value, visitor)
    }

    /// Hand an already-parsed number to `visitor`.
    fn dispatch<V: Visitor<'de>>(value: Num, visitor: V) -> Result<V::Value> {
        match value {
            Num::Unsigned(value) => visitor.visit_u64(value),
            Num::Signed(value) => visitor.visit_i64(value),
            Num::NegZero => visitor.visit_f64(-0.0),
            Num::Float(value) => visitor.visit_f64(value),
        }
    }

    /// Consume the closing bracket of an array or object.
    ///
    /// Anything else means the visitor stopped early with elements left over —
    /// a three-element JSON array read into a `[u8; 2]`, say — which is an
    /// error, not a truncation.
    fn end_container(&mut self, close: u8, what: &str) -> Result<()> {
        if self.peek_value()? == close {
            self.bump();
            Ok(())
        } else {
            Err(self.err(format!("expected {what}")))
        }
    }

    /// The shared body of every integer entry point: a number goes to
    /// [`Self::visit_number`] in integer context, anything else falls back to
    /// the generic path so the visitor reports the type error itself.
    fn deserialize_integer<V: Visitor<'de>>(&mut self, visitor: V) -> Result<V::Value> {
        if matches!(self.peek_value()?, b'-' | b'0'..=b'9') {
            self.visit_number(visitor)
        } else {
            serde::de::Deserializer::deserialize_any(&mut *self, visitor)
        }
    }

    /// The 128-bit entry points, which are the only ones that can carry an
    /// integer the `u64`/`i64` classification cannot hold.
    ///
    /// Without this, `deserialize_u128` went through the same `f64` widening as
    /// every other integer, so `u128::MAX` — which this crate SERIALIZES
    /// exactly — read back as an error, and `to_value` of it silently became a
    /// rounded float. `serde_json` round-trips both exactly.
    fn deserialize_wide<V: Visitor<'de>>(&mut self, visitor: V, signed: bool) -> Result<V::Value> {
        if !matches!(self.peek_value()?, b'-' | b'0'..=b'9') {
            return serde::de::Deserializer::deserialize_any(&mut *self, visitor);
        }
        let parsed = self.parse_number()?;
        if !parsed.floating {
            if signed {
                if let Ok(wide) = parsed.text.parse::<i128>() {
                    return visitor.visit_i128(wide);
                }
            } else if let Ok(wide) = parsed.text.parse::<u128>() {
                return visitor.visit_u128(wide);
            }
        }
        // Not an integer of this signedness and width: the ordinary dispatch
        // reports the type error the visitor would have reported anyway.
        Self::dispatch(parsed.value, visitor)
    }

    /// Consume one value without materialising it — the ignored-field path.
    fn skip_value(&mut self) -> Result<()> {
        match self.peek_value()? {
            b'n' => self.literal(b"null", "null"),
            b't' => self.literal(b"true", "true"),
            b'f' => self.literal(b"false", "false"),
            b'"' => {
                self.bump();
                self.parse_string_body().map(|_| ())
            }
            b'-' | b'0'..=b'9' => self.parse_number().map(|_| ()),
            b'[' => {
                self.enter()?;
                self.bump();
                let mut first = true;
                loop {
                    if self.peek_value()? == b']' {
                        self.bump();
                        self.leave();
                        return Ok(());
                    }
                    if !first {
                        self.expect(b',', "`,` or `]`")?;
                        // A trailing comma is not JSON.
                        if self.peek_value()? == b']' {
                            return Err(self.err("trailing comma in array"));
                        }
                    }
                    first = false;
                    self.skip_value()?;
                }
            }
            b'{' => {
                self.enter()?;
                self.bump();
                let mut first = true;
                loop {
                    if self.peek_value()? == b'}' {
                        self.bump();
                        self.leave();
                        return Ok(());
                    }
                    if !first {
                        self.expect(b',', "`,` or `}`")?;
                        if self.peek_value()? == b'}' {
                            return Err(self.err("trailing comma in object"));
                        }
                    }
                    first = false;
                    self.expect(b'"', "an object key")?;
                    self.parse_string_body()?;
                    self.expect(b':', "`:`")?;
                    self.skip_value()?;
                }
            }
            _ => Err(self.err("expected a JSON value")),
        }
    }
}

/// A classified JSON number.
enum Num {
    Unsigned(u64),
    Signed(i64),
    /// `-0` written with no fraction and no exponent.
    NegZero,
    Float(f64),
}

/// A number as the parser saw it: the classification, the exact source text,
/// and whether that text carried a fraction or an exponent.
struct Parsed<'a> {
    /// How the number classifies for the ordinary visitor dispatch.
    value: Num,
    /// The exact characters the number was written as.
    text: &'a str,
    /// Whether a `.` or an `e`/`E` appeared, which makes it a float regardless
    /// of how small it is.
    floating: bool,
}

impl<'de> de::Deserializer<'de> for &mut Deserializer<'de> {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        match self.peek_value()? {
            b'n' => {
                self.literal(b"null", "null")?;
                visitor.visit_unit()
            }
            b't' => {
                self.literal(b"true", "true")?;
                visitor.visit_bool(true)
            }
            b'f' => {
                self.literal(b"false", "false")?;
                visitor.visit_bool(false)
            }
            b'"' => {
                self.bump();
                match self.parse_string_body()? {
                    Cow::Borrowed(text) => visitor.visit_borrowed_str(text),
                    Cow::Owned(text) => visitor.visit_string(text),
                }
            }
            b'-' | b'0'..=b'9' => self.visit_number(visitor),
            b'[' => {
                self.enter()?;
                self.bump();
                let out = visitor.visit_seq(SeqAccess {
                    de: self,
                    first: true,
                });
                // The closing bracket is consumed HERE, not by the access, and
                // that placement is load-bearing. A `Vec` visitor drains the
                // sequence, but the visitor for `[u8; 8]` — or any tuple —
                // takes exactly N elements and returns, so an access that ate
                // the `]` on the way out would work for one and leave a stray
                // bracket for the other. Closing outside means both end in the
                // same state, and it is also what makes `[1,2,3]` into a
                // two-element array an error rather than a silent truncation.
                let out = match out {
                    Ok(value) => self.end_container(b']', "`,` or `]`").map(|()| value),
                    Err(error) => Err(error),
                };
                self.leave();
                out
            }
            b'{' => {
                self.enter()?;
                self.bump();
                let out = visitor.visit_map(MapAccess {
                    de: self,
                    first: true,
                });
                let out = match out {
                    Ok(value) => self.end_container(b'}', "`,` or `}`").map(|()| value),
                    Err(error) => Err(error),
                };
                self.leave();
                out
            }
            _ => Err(self.err("expected a JSON value")),
        }
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        if self.peek_value()? == b'n' {
            self.literal(b"null", "null")?;
            visitor.visit_none()
        } else {
            visitor.visit_some(self)
        }
    }

    fn deserialize_unit<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        self.peek_value()?;
        self.literal(b"null", "null")?;
        visitor.visit_unit()
    }

    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value> {
        self.deserialize_unit(visitor)
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value> {
        visitor.visit_newtype_struct(self)
    }

    /// A struct is a JSON object, or — as `serde_json` also allows — a JSON
    /// array of its fields in declaration order.
    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value> {
        self.deserialize_any(visitor)
    }

    /// An enum is either a bare string (a unit variant) or a single-entry
    /// object mapping the variant name to its payload.
    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value> {
        match self.peek_value()? {
            b'"' => {
                self.bump();
                let name = self.parse_string_body()?;
                visitor.visit_enum(name.into_deserializer())
            }
            b'{' => {
                self.enter()?;
                self.bump();
                let out = visitor.visit_enum(EnumAccess { de: self })?;
                self.skip_ws();
                self.expect(b'}', "`}` after the enum variant")?;
                self.leave();
                Ok(out)
            }
            _ => Err(self.err("expected a string or an object for an enum")),
        }
    }

    fn deserialize_ignored_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        self.skip_value()?;
        visitor.visit_unit()
    }

    fn deserialize_i8<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        self.deserialize_integer(visitor)
    }
    fn deserialize_i16<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        self.deserialize_integer(visitor)
    }
    fn deserialize_i32<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        self.deserialize_integer(visitor)
    }
    fn deserialize_i64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        self.deserialize_integer(visitor)
    }
    fn deserialize_i128<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        self.deserialize_wide(visitor, true)
    }
    fn deserialize_u8<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        self.deserialize_integer(visitor)
    }
    fn deserialize_u16<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        self.deserialize_integer(visitor)
    }
    fn deserialize_u32<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        self.deserialize_integer(visitor)
    }
    fn deserialize_u64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        self.deserialize_integer(visitor)
    }
    fn deserialize_u128<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        self.deserialize_wide(visitor, false)
    }

    serde::forward_to_deserialize_any! {
        bool f32 f64 char str string bytes byte_buf seq tuple tuple_struct map
        identifier
    }
}

struct SeqAccess<'a, 'de> {
    de: &'a mut Deserializer<'de>,
    first: bool,
}

impl<'de> de::SeqAccess<'de> for SeqAccess<'_, 'de> {
    type Error = Error;

    fn next_element_seed<T: de::DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>> {
        // `]` ends the sequence but is NOT consumed here — see the note in
        // `deserialize_any`.
        let byte = self.de.peek_value()?;
        if byte == b']' {
            return Ok(None);
        }
        if self.first {
            self.first = false;
        } else if byte == b',' {
            self.de.bump();
            if self.de.peek_value()? == b']' {
                return Err(self.de.err("trailing comma in array"));
            }
        } else {
            return Err(self.de.err("expected `,` or `]`"));
        }
        seed.deserialize(&mut *self.de).map(Some)
    }
}

struct MapAccess<'a, 'de> {
    de: &'a mut Deserializer<'de>,
    first: bool,
}

impl<'de> de::MapAccess<'de> for MapAccess<'_, 'de> {
    type Error = Error;

    fn next_key_seed<K: de::DeserializeSeed<'de>>(&mut self, seed: K) -> Result<Option<K::Value>> {
        // `}` ends the object but is NOT consumed here — same reason as
        // `SeqAccess`, so a visitor that stops early and one that drains leave
        // the parser in the same place.
        let byte = self.de.peek_value()?;
        if byte == b'}' {
            return Ok(None);
        }
        if self.first {
            self.first = false;
        } else if byte == b',' {
            self.de.bump();
            if self.de.peek_value()? == b'}' {
                return Err(self.de.err("trailing comma in object"));
            }
        } else {
            return Err(self.de.err("expected `,` or `}`"));
        }
        self.de.skip_ws();
        if self.de.peek() != Some(b'"') {
            return Err(self.de.err("expected an object key"));
        }
        seed.deserialize(KeyDeserializer { de: self.de }).map(Some)
    }

    fn next_value_seed<V: de::DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value> {
        self.de.expect(b':', "`:` after an object key")?;
        seed.deserialize(&mut *self.de)
    }
}

/// Object keys are always JSON strings, so the key deserializer is the string
/// parser and nothing else — a numeric key type still arrives as text and is
/// re-parsed by `serde`'s own integer-from-string handling.
struct KeyDeserializer<'a, 'de> {
    de: &'a mut Deserializer<'de>,
}

impl<'de> de::Deserializer<'de> for KeyDeserializer<'_, 'de> {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        self.de.expect(b'"', "an object key")?;
        match self.de.parse_string_body()? {
            Cow::Borrowed(text) => visitor.visit_borrowed_str(text),
            Cow::Owned(text) => visitor.visit_string(text),
        }
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value> {
        self.de.expect(b'"', "an object key")?;
        let name = self.de.parse_string_body()?;
        visitor.visit_enum(name.into_deserializer())
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf option unit unit_struct newtype_struct seq tuple
        tuple_struct map struct identifier ignored_any
    }
}

struct EnumAccess<'a, 'de> {
    de: &'a mut Deserializer<'de>,
}

impl<'de> de::EnumAccess<'de> for EnumAccess<'_, 'de> {
    type Error = Error;
    type Variant = Self;

    fn variant_seed<V: de::DeserializeSeed<'de>>(self, seed: V) -> Result<(V::Value, Self)> {
        self.de.skip_ws();
        if self.de.peek() != Some(b'"') {
            return Err(self.de.err("expected an enum variant name"));
        }
        let value = seed.deserialize(KeyDeserializer { de: self.de })?;
        self.de.expect(b':', "`:` after the enum variant name")?;
        Ok((value, self))
    }
}

impl<'de> de::VariantAccess<'de> for EnumAccess<'_, 'de> {
    type Error = Error;

    fn unit_variant(self) -> Result<()> {
        Err(self.de.err("expected a unit variant to be a bare string"))
    }

    fn newtype_variant_seed<T: de::DeserializeSeed<'de>>(self, seed: T) -> Result<T::Value> {
        seed.deserialize(&mut *self.de)
    }

    fn tuple_variant<V: Visitor<'de>>(self, _len: usize, visitor: V) -> Result<V::Value> {
        de::Deserializer::deserialize_any(&mut *self.de, visitor)
    }

    fn struct_variant<V: Visitor<'de>>(
        self,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value> {
        de::Deserializer::deserialize_any(&mut *self.de, visitor)
    }
}

/// Deserialize a `T` from JSON bytes, requiring that the whole input is ONE
/// value.
///
/// # Errors
/// Any syntax error, any `Deserialize` failure, nesting past
/// [`RECURSION_LIMIT`], or trailing data after the value.
pub fn from_slice<'de, T: serde::Deserialize<'de>>(input: &'de [u8]) -> Result<T> {
    let mut de = Deserializer::from_slice(input);
    let value = T::deserialize(&mut de)?;
    de.end()?;
    Ok(value)
}

/// Deserialize a `T` from a JSON string, borrowing from it where the target
/// type allows.
///
/// # Errors
/// As [`from_slice`].
pub fn from_str<'de, T: serde::Deserialize<'de>>(input: &'de str) -> Result<T> {
    let mut de = Deserializer::from_slice(input.as_bytes());
    let value = T::deserialize(&mut de)?;
    de.end()?;
    Ok(value)
}
