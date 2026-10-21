// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! One record, two syntaxes: a producer writes each field once, as
//! `s.<kind>("key", value)` with the key beside its value, and a [`FieldSink`]
//! spells it. [`TextSink`] spells the control wire's `key=value` line,
//! [`JsonSink`] one JSON object; a record with both forms drives either from
//! the same emit function, so its two forms cannot disagree on a key, an order
//! or a value.
//!
//! The spellings, per kind:
//!
//! | kind | text | JSON |
//! |---|---|---|
//! | [`count`](FieldSink::count) | `7` | `7` |
//! | [`decimal`](FieldSink::decimal) | `1.25` (the value's places) | `1.25` |
//! | [`flag`](FieldSink::flag) | the sink's [`FlagWords`] | `true` / `false` |
//! | [`label`](FieldSink::label) | one token, percent-escaped ([`crate::pct`]) | a string ([`crate::json`]) |
//! | [`absent`](FieldSink::absent) | the word given (`none`, `busy`, `-`) | `null` |
//! | [`pairs`](FieldSink::pairs) | `a:1,b:2`, or the empty word | `{"a":1,"b":2}` |
//!
//! A [`Cell::Rec`] value inside pairs is `1/2/3` in text (its keys are the
//! JSON form's alone) and `{"arms":1,"past":2}` in JSON. A pair key in text is
//! percent-escaped with its separators `,` and `:` as well, so `k:v,k:v`
//! splits back into the keys written whatever bytes they hold.

use std::fmt::Write as _;

/// How a text record spells a flag. Records differ (`metrics` says `0`/`1`,
/// a status line `true`/`false`, the privacy report `yes`/`no`), so the record
/// chooses when it builds its [`TextSink`]; JSON always says `true`/`false`.
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

/// A number written with a fixed count of decimal places (`{:.N}`), the same
/// digits in both syntaxes. A non-finite value is `null` in JSON, which has no
/// spelling for it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Decimal {
    value: f64,
    places: u8,
}

impl Decimal {
    /// `value`, written to `places` decimal places.
    #[must_use]
    pub const fn new(value: f64, places: u8) -> Self {
        Self { value, places }
    }

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

/// The fields of one record, in order. Keys are the producer's constants and
/// are written as given (JSON escapes them); every runtime string goes through
/// [`label`](Self::label) or a [`pairs`](Self::pairs) key, which each syntax
/// escapes its own way.
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
    /// A self-labelling ledger: `k:v,...` in text (`empty_text` when there are
    /// no pairs; each `k` escaped so it holds no `,` or `:`), an object in JSON
    /// (`{}` when empty).
    fn pairs(&mut self, key: &str, pairs: &[(&str, Cell<'_>)], empty_text: &'static str);

    /// [`count`](Self::count) when `Some`, else [`absent`](Self::absent).
    fn opt_count(&mut self, key: &str, v: Option<u64>, absent_text: &'static str) {
        match v {
            Some(v) => self.count(key, v),
            None => self.absent(key, absent_text),
        }
    }

    /// [`decimal`](Self::decimal) when `Some`, else [`absent`](Self::absent).
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

/// The control wire's text form: `<head> key=value key=value ...`, every value
/// one whitespace-free token as long as the producer's absent and empty words
/// are.
#[derive(Debug)]
pub struct TextSink {
    out: String,
    flags: FlagWords,
}

impl TextSink {
    /// A line that starts with `head` (`OK`, or empty for a fragment) and
    /// spells flags as `flags`.
    #[must_use]
    pub fn new(head: &str, flags: FlagWords) -> Self {
        Self {
            out: head.to_string(),
            flags,
        }
    }

    /// The line so far, with no trailing newline.
    #[must_use]
    pub fn finish(self) -> String {
        self.out
    }

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

/// A pair key in the text form: [`crate::pct`]'s escape, plus `%2C` for `,`
/// and `%3A` for `:`, the separators of `k:v,k:v`. A label keeps a bare `,`
/// (a comma-joined list is one label), so this applies to pair keys alone.
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
    fn default() -> Self {
        Self::new()
    }
}

impl JsonSink {
    /// An empty object.
    #[must_use]
    pub fn new() -> Self {
        Self {
            out: String::from("{"),
            first: true,
        }
    }

    /// The closed object.
    #[must_use]
    pub fn finish(mut self) -> String {
        self.out.push('}');
        self.out
    }

    fn key(&mut self, key: &str) {
        if !std::mem::take(&mut self.first) {
            self.out.push(',');
        }
        self.string(key);
        self.out.push(':');
    }

    fn string(&mut self, s: &str) {
        self.out.push('"');
        crate::json::escape_into(&mut self.out, s);
        self.out.push('"');
    }

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
mod tests {
    use super::*;

    fn text(flags: FlagWords, emit: impl FnOnce(&mut dyn FieldSink)) -> String {
        let mut sink = TextSink::new("OK", flags);
        emit(&mut sink);
        sink.finish()
    }

    fn json(emit: impl FnOnce(&mut dyn FieldSink)) -> String {
        let mut sink = JsonSink::new();
        emit(&mut sink);
        sink.finish()
    }

    /// One emit and the text and JSON bytes it must produce.
    type Case<'a> = (&'a dyn Fn(&mut dyn FieldSink), &'a str, &'a str);

    /// Each method's exact bytes in each syntax, one field per call.
    #[test]
    fn each_kind_spells_its_documented_bytes() {
        let rec = [("arms", Cell::Count(3)), ("past", Cell::Count(1))];
        let cases: [Case<'_>; 12] = [
            (&|s| s.count("frames", 42), " frames=42", "\"frames\":42"),
            (
                &|s| s.decimal("p99_ms", Decimal::new(1.005_1, 2)),
                " p99_ms=1.01",
                "\"p99_ms\":1.01",
            ),
            (
                &|s| s.decimal("threshold_ms", Decimal::new(33.333_333, 1)),
                " threshold_ms=33.3",
                "\"threshold_ms\":33.3",
            ),
            (
                &|s| s.decimal("lead_ms", Decimal::new(-0.004, 2)),
                " lead_ms=-0.00",
                "\"lead_ms\":-0.00",
            ),
            (&|s| s.flag("valid", true), " valid=1", "\"valid\":true"),
            (&|s| s.flag("valid", false), " valid=0", "\"valid\":false"),
            (
                &|s| s.label("backend", "gpu"),
                " backend=gpu",
                "\"backend\":\"gpu\"",
            ),
            (&|s| s.absent("rows", "busy"), " rows=busy", "\"rows\":null"),
            (
                &|s| s.count_pairs("fires", &[("frame_cap", 2), ("blink", 7)], "none"),
                " fires=frame_cap:2,blink:7",
                "\"fires\":{\"frame_cap\":2,\"blink\":7}",
            ),
            (
                &|s| s.count_pairs("fires", &[], "none"),
                " fires=none",
                "\"fires\":{}",
            ),
            (
                &|s| s.pairs("arms", &[("cursor", Cell::Rec(&rec))], "none"),
                " arms=cursor:3/1",
                "\"arms\":{\"cursor\":{\"arms\":3,\"past\":1}}",
            ),
            (
                &|s| {
                    s.opt_count("cols", None, "busy");
                    s.opt_decimal("birth_ms", Some(Decimal::new(2.5, 2)), "none");
                },
                " cols=busy birth_ms=2.50",
                "\"cols\":null,\"birth_ms\":2.50",
            ),
        ];
        for (emit, want_text, want_json) in cases {
            assert_eq!(text(FlagWords::ZeroOne, emit), format!("OK{want_text}"));
            assert_eq!(json(emit), format!("{{{want_json}}}"));
        }
    }

    /// Fields join with one space in text and one comma in JSON, in call order.
    #[test]
    fn fields_keep_their_call_order() {
        let emit = |s: &mut dyn FieldSink| {
            s.count("b", 2);
            s.count("a", 1);
            s.flag("c", true);
        };
        assert_eq!(text(FlagWords::ZeroOne, emit), "OK b=2 a=1 c=1");
        assert_eq!(json(emit), "{\"b\":2,\"a\":1,\"c\":true}");
        assert_eq!(TextSink::new("", FlagWords::ZeroOne).finish(), "");
        assert_eq!(JsonSink::new().finish(), "{}");
    }

    /// The record picks its text flag words; JSON is always a boolean.
    #[test]
    fn the_text_flag_words_are_the_records_choice() {
        for (flags, yes, no) in [
            (FlagWords::ZeroOne, "1", "0"),
            (FlagWords::TrueFalse, "true", "false"),
            (FlagWords::YesNo, "yes", "no"),
        ] {
            let emit = |s: &mut dyn FieldSink| {
                s.flag("on", true);
                s.flag("off", false);
            };
            assert_eq!(text(flags, emit), format!("OK on={yes} off={no}"));
            assert_eq!(json(emit), "{\"on\":true,\"off\":false}");
        }
    }

    /// A runtime string is escaped by each syntax's own rule: a quote and a
    /// backslash are escaped in JSON and pass through the text form bare (they
    /// are graphic), while a space or a control byte is `%XX` in text, so the
    /// value stays one token, and `\u00XX` in JSON. A pair key's `,` and `:`
    /// are `%2C` and `%3A` in text, so its ledger splits back into the keys
    /// written, while a label keeps its `,` and `:` bare. The control: a plain
    /// identifier is the same bytes in both.
    #[test]
    fn labels_and_pair_keys_are_escaped_per_syntax() {
        let emit = |s: &mut dyn FieldSink| {
            s.label("plain", "frame_cap");
            s.label("quoted", "a\"b\\c");
            s.label("spaced", "two words\u{1b}");
            s.label("listed", "a:b,c");
            s.count_pairs("ledger", &[("a b", 1), ("a:b", 2), ("c,d", 3)], "none");
        };
        let line = text(FlagWords::ZeroOne, emit);
        assert_eq!(
            line,
            "OK plain=frame_cap quoted=a\"b\\c spaced=two%20words%1B listed=a:b,c \
             ledger=a%20b:1,a%3Ab:2,c%2Cd:3"
        );
        assert_eq!(
            json(emit),
            "{\"plain\":\"frame_cap\",\"quoted\":\"a\\\"b\\\\c\",\
             \"spaced\":\"two words\\u001b\",\"listed\":\"a:b,c\",\
             \"ledger\":{\"a b\":1,\"a:b\":2,\"c,d\":3}}"
        );
        let ledger = line.rsplit_once("ledger=").map_or("", |(_, v)| v);
        let keys: Vec<String> = ledger
            .split(',')
            .filter_map(|pair| pair.split_once(':'))
            .map(|(k, _)| crate::pct::decode(k))
            .collect();
        assert_eq!(keys, ["a b", "a:b", "c,d"], "{ledger}");
    }

    /// JSON has no spelling for a non-finite number, so it writes `null`; the
    /// text form writes what `{:.N}` writes. The control: a finite value is
    /// the same digits in both.
    #[test]
    fn a_non_finite_decimal_is_null_in_json() {
        let emit = |s: &mut dyn FieldSink| {
            s.decimal("nan", Decimal::new(f64::NAN, 2));
            s.decimal("ok", Decimal::new(0.5, 2));
        };
        assert_eq!(text(FlagWords::ZeroOne, emit), "OK nan=NaN ok=0.50");
        assert_eq!(json(emit), "{\"nan\":null,\"ok\":0.50}");
    }
}
