// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Dual-syntax record field formatting for `key=value` text lines and JSON objects.

use std::fmt::Write as _;

/// How a text record spells a boolean flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlagWords {
    /// `1` / `0`.
    ZeroOne,
    /// `true` / `false`.
    TrueFalse,
    /// `yes` / `no`.
    YesNo,
}

impl FlagWords {
    #[inline]
    const fn word(self, v: bool) -> &'static str {
        match (self, v) {
            (Self::ZeroOne, true) => "1",
            (Self::ZeroOne, false) => "0",
            (Self::TrueFalse, true) => "true",
            (Self::TrueFalse, false) => "false",
            (Self::YesNo, true) => "yes",
            (Self::YesNo, false) => "no",
        }
    }
}

/// A number formatted with a fixed count of decimal places.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Decimal {
    value: f64,
    places: u8,
}

impl Decimal {
    /// `value`, written to `places` decimal places.
    #[must_use]
    #[inline]
    pub const fn new(value: f64, places: u8) -> Self {
        Self { value, places }
    }

    #[inline]
    fn write(self, out: &mut String) {
        let _ = write!(out, "{:.*}", usize::from(self.places), self.value);
    }
}

/// One value inside [`FieldSink::pairs`].
#[derive(Clone, Copy, Debug)]
pub enum Cell<'a> {
    /// An integer.
    Count(u64),
    /// A fixed-place decimal.
    Decimal(Decimal),
    /// A small record: `a/b/c` in text, an object in JSON.
    Rec(&'a [(&'a str, Cell<'a>)]),
}

/// Sink receiving the fields of one record in order.
pub trait FieldSink {
    /// An integer.
    fn count(&mut self, key: &str, v: u64);
    /// A fixed-place decimal.
    fn decimal(&mut self, key: &str, v: Decimal);
    /// A boolean.
    fn flag(&mut self, key: &str, v: bool);
    /// A runtime string: one escaped token in text, a JSON string.
    fn label(&mut self, key: &str, v: &str);
    /// No value: `text` (`none`, `busy`, `-`) in the text form, `null` in JSON.
    fn absent(&mut self, key: &str, text: &'static str);
    /// Writes a key-value ledger as `k:v,...` in text (`empty_text` when empty) and an object in JSON.
    fn pairs(&mut self, key: &str, pairs: &[(&str, Cell<'_>)], empty_text: &'static str);

    /// [`count`](Self::count) when `Some`, else [`absent`](Self::absent).
    #[inline]
    fn opt_count(&mut self, key: &str, v: Option<u64>, absent_text: &'static str) {
        match v {
            Some(v) => self.count(key, v),
            None => self.absent(key, absent_text),
        }
    }

    /// [`decimal`](Self::decimal) when `Some`, else [`absent`](Self::absent).
    #[inline]
    fn opt_decimal(&mut self, key: &str, v: Option<Decimal>, absent_text: &'static str) {
        match v {
            Some(v) => self.decimal(key, v),
            None => self.absent(key, absent_text),
        }
    }

    /// [`pairs`](Self::pairs) of plain counts.
    fn count_pairs(&mut self, key: &str, pairs: &[(&str, u64)], empty_text: &'static str) {
        let cells: Vec<(&str, Cell<'_>)> = pairs
            .iter()
            .map(|&(name, n)| (name, Cell::Count(n)))
            .collect();
        self.pairs(key, &cells, empty_text);
    }
}

/// The control wire's `<head> key=value ...` text formatter.
#[derive(Debug)]
pub struct TextSink {
    out: String,
    flags: FlagWords,
}

impl TextSink {
    /// Creates a line starting with `head` that formats flags with `flags`.
    #[must_use]
    #[inline]
    pub fn new(head: &str, flags: FlagWords) -> Self {
        Self {
            out: head.to_string(),
            flags,
        }
    }

    /// The line so far, with no trailing newline.
    #[must_use]
    #[inline]
    pub fn finish(self) -> String {
        self.out
    }

    #[inline]
    fn key(&mut self, key: &str) {
        self.out.push(' ');
        self.out.push_str(key);
        self.out.push('=');
    }

    fn cell(&mut self, cell: &Cell<'_>) {
        match cell {
            Cell::Count(n) => {
                let _ = write!(self.out, "{n}");
            }
            Cell::Decimal(d) => d.write(&mut self.out),
            Cell::Rec(fields) => {
                for (i, (_, v)) in fields.iter().enumerate() {
                    if i > 0 {
                        self.out.push('/');
                    }
                    self.cell(v);
                }
            }
        }
    }
}

impl FieldSink for TextSink {
    fn count(&mut self, key: &str, v: u64) {
        self.key(key);
        let _ = write!(self.out, "{v}");
    }

    fn decimal(&mut self, key: &str, v: Decimal) {
        self.key(key);
        v.write(&mut self.out);
    }

    fn flag(&mut self, key: &str, v: bool) {
        self.key(key);
        self.out.push_str(self.flags.word(v));
    }

    fn label(&mut self, key: &str, v: &str) {
        self.key(key);
        crate::pct::encode_into(&mut self.out, v);
    }

    fn absent(&mut self, key: &str, text: &'static str) {
        self.key(key);
        self.out.push_str(text);
    }

    fn pairs(&mut self, key: &str, pairs: &[(&str, Cell<'_>)], empty_text: &'static str) {
        self.key(key);
        if pairs.is_empty() {
            self.out.push_str(empty_text);
            return;
        }
        for (i, (name, v)) in pairs.iter().enumerate() {
            if i > 0 {
                self.out.push(',');
            }
            encode_pair_key(&mut self.out, name);
            self.out.push(':');
            self.cell(v);
        }
    }
}

fn encode_pair_key(out: &mut String, key: &str) {
    let mut buf = [0; 4];
    for c in key.chars() {
        match c {
            ',' => out.push_str("%2C"),
            ':' => out.push_str("%3A"),
            c => crate::pct::encode_into(out, c.encode_utf8(&mut buf)),
        }
    }
}

/// One JSON object, `{"key":value,...}`, on one line.
#[derive(Debug)]
pub struct JsonSink {
    out: String,
    first: bool,
}

impl Default for JsonSink {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl JsonSink {
    /// An empty object.
    #[must_use]
    #[inline]
    pub fn new() -> Self {
        Self {
            out: String::from("{"),
            first: true,
        }
    }

    /// The closed object.
    #[must_use]
    #[inline]
    pub fn finish(mut self) -> String {
        self.out.push('}');
        self.out
    }

    #[inline]
    fn key(&mut self, key: &str) {
        if !std::mem::take(&mut self.first) {
            self.out.push(',');
        }
        self.string(key);
        self.out.push(':');
    }

    #[inline]
    fn string(&mut self, s: &str) {
        self.out.push('"');
        crate::json::escape_into(&mut self.out, s);
        self.out.push('"');
    }

    #[inline]
    fn decimal_value(&mut self, d: Decimal) {
        if d.value.is_finite() {
            d.write(&mut self.out);
        } else {
            self.out.push_str("null");
        }
    }

    fn object(&mut self, fields: &[(&str, Cell<'_>)]) {
        self.out.push('{');
        for (i, (name, v)) in fields.iter().enumerate() {
            if i > 0 {
                self.out.push(',');
            }
            self.string(name);
            self.out.push(':');
            self.cell(v);
        }
        self.out.push('}');
    }

    fn cell(&mut self, cell: &Cell<'_>) {
        match cell {
            Cell::Count(n) => {
                let _ = write!(self.out, "{n}");
            }
            Cell::Decimal(d) => self.decimal_value(*d),
            Cell::Rec(fields) => self.object(fields),
        }
    }
}

impl FieldSink for JsonSink {
    fn count(&mut self, key: &str, v: u64) {
        self.key(key);
        let _ = write!(self.out, "{v}");
    }

    fn decimal(&mut self, key: &str, v: Decimal) {
        self.key(key);
        self.decimal_value(v);
    }

    fn flag(&mut self, key: &str, v: bool) {
        self.key(key);
        self.out.push_str(if v { "true" } else { "false" });
    }

    fn label(&mut self, key: &str, v: &str) {
        self.key(key);
        self.string(v);
    }

    fn absent(&mut self, key: &str, _text: &'static str) {
        self.key(key);
        self.out.push_str("null");
    }

    fn pairs(&mut self, key: &str, pairs: &[(&str, Cell<'_>)], _empty_text: &'static str) {
        self.key(key);
        self.object(pairs);
    }
}

#[cfg(test)]
#[path = "fields_tests.rs"]
mod tests;
