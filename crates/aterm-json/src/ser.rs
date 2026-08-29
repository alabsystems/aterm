// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `serde::Serializer` and the JSON writer under it.
//!
//! Output is compact — no spaces, no newlines — and byte-identical to
//! `serde_json`'s, which matters because some of it is signed, some is hashed
//! into a checkpoint fingerprint, and some is a request body a server reads.
//!
//! The two places where "valid JSON" leaves a choice, and the choice made:
//!
//! * **non-finite floats become `null`.** JSON has no NaN and no infinity;
//!   refusing to serialize would turn a rendering glitch into a failed request,
//!   so the value that IS representable is written, exactly as `serde_json`
//!   does.
//! * **only ASCII control characters are escaped.** A non-ASCII character is
//!   written as UTF-8, not as a `\u` escape — again matching `serde_json`, and
//!   the reason the output of the two is comparable byte for byte at all.

use serde::{Serialize, ser};

use crate::error::{Error, Result};

/// Serialize `value` as compact JSON bytes.
///
/// # Errors
/// Only from the `Serialize` impl itself — a map with a non-string key, or a
/// type that reports its own failure.
pub fn to_vec<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(128);
    value.serialize(Serializer { out: &mut out })?;
    Ok(out)
}

/// Serialize `value` as a compact JSON string.
///
/// # Errors
/// As [`to_vec`].
pub fn to_string<T: Serialize + ?Sized>(value: &T) -> Result<String> {
    let bytes = to_vec(value)?;
    // Every byte the writer emits is either ASCII or a byte of a `&str` it was
    // handed, so the result is always UTF-8; the check keeps that local.
    String::from_utf8(bytes).map_err(|_| Error::message("serializer produced invalid UTF-8"))
}

/// Write `text` as a JSON string literal, quotes included.
pub(crate) fn write_json_string(out: &mut Vec<u8>, text: &str) {
    out.push(b'"');
    let bytes = text.as_bytes();
    let mut chunk = 0usize;
    for (i, &byte) in bytes.iter().enumerate() {
        let escape: &[u8] = match byte {
            b'"' => b"\\\"",
            b'\\' => b"\\\\",
            0x08 => b"\\b",
            0x0C => b"\\f",
            b'\n' => b"\\n",
            b'\r' => b"\\r",
            b'\t' => b"\\t",
            0x00..=0x1F => b"",
            _ => continue,
        };
        if let Some(run) = bytes.get(chunk..i) {
            out.extend_from_slice(run);
        }
        if escape.is_empty() {
            // Any other C0 control: the six-character form.
            out.extend_from_slice(b"\\u00");
            out.push(hex_nibble(byte >> 4));
            out.push(hex_nibble(byte & 0x0F));
        } else {
            out.extend_from_slice(escape);
        }
        chunk = i.saturating_add(1);
    }
    if let Some(run) = bytes.get(chunk..) {
        out.extend_from_slice(run);
    }
    out.push(b'"');
}

fn hex_nibble(v: u8) -> u8 {
    // `v <= 15` at every call (both arguments are masked or shifted), so the
    // lookup is in range; `get` keeps that local and the fallback is dead.
    *b"0123456789abcdef".get(v as usize).unwrap_or(&b'0')
}

/// Write an unsigned integer in decimal.
pub(crate) fn write_u128(out: &mut Vec<u8>, value: u128) {
    // u128 is at most 39 decimal digits.
    let mut buf = [0u8; 39];
    let mut v = value;
    let mut i = buf.len();
    while i > 0 {
        i -= 1;
        if let Some(slot) = buf.get_mut(i) {
            *slot = b'0'.saturating_add((v % 10) as u8);
        }
        v /= 10;
        if v == 0 {
            break;
        }
    }
    if let Some(digits) = buf.get(i..) {
        out.extend_from_slice(digits);
    }
}

/// Write a signed integer in decimal.
pub(crate) fn write_i128(out: &mut Vec<u8>, value: i128) {
    if value < 0 {
        out.push(b'-');
    }
    // `unsigned_abs` is exact for i128::MIN, where negation would overflow.
    write_u128(out, value.unsigned_abs());
}

/// The shortest decimal digit string that reads back as `value`, correctly
/// rounded, with the power of ten it is scaled by.
///
/// "Shortest" alone is not enough to match `serde_json`. Rust's own shortest
/// formatter (`{:e}`) guarantees the digits ROUND-TRIP but not that they are
/// the closest decimal of that length: for `0x42ea3c464c15f354` it emits
/// `...29063` where the closest 17-digit decimal is `...29062`. `ryu`, which
/// `serde_json` formats with, always picks the closest. Asking `{:.*e}` for an
/// explicit precision gives correctly-rounded digits, so walking the precision
/// up from one digit and stopping at the first that round-trips yields the
/// shortest correctly-rounded form — which is exactly what `ryu` produces.
fn shortest_f64(value: f64) -> (String, i32) {
    for precision in 0..=17usize {
        let text = format!("{value:.precision$e}");
        if text.parse::<f64>() == Ok(value) {
            return split_exp(&text);
        }
    }
    split_exp(&format!("{value:.17e}"))
}

/// As [`shortest_f64`], at `f32` width: `0.1f32` must print as `0.1`, not as
/// the `f64` it widens to.
fn shortest_f32(value: f32) -> (String, i32) {
    for precision in 0..=9usize {
        let text = format!("{value:.precision$e}");
        if text.parse::<f32>() == Ok(value) {
            return split_exp(&text);
        }
    }
    split_exp(&format!("{value:.9e}"))
}

/// Split `d.ddde±X` into its digits (decimal point removed) and its exponent.
fn split_exp(text: &str) -> (String, i32) {
    let (mantissa, exponent) = text.split_once('e').unwrap_or((text, "0"));
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    (digits, exponent.parse::<i32>().unwrap_or(0))
}

/// Lay the digits out the way `serde_json` lays them out, with `kk` the count
/// of digits before the decimal point (`10^(kk-1) <= |v| < 10^kk`):
///
/// * `kk` inside `fixed_range` → fixed notation, gaining a `.0` when there is
///   no fractional part (`1e15` is `1000000000000000.0`, never
///   `1000000000000000`, which JSON would read back as an integer);
/// * otherwise → `d.ddde±XX`, the exponent signed even when positive.
///
/// The range depends on the WIDTH, and asymmetrically so: `ryu`'s pretty
/// printer switches to exponential above `kk = 16` for a double but above
/// `kk = 13` for a single, and below `kk = -4` for a double but `kk = -5` for a
/// single. Both boundaries were read off the reference rather than guessed
/// (`1e15` is fixed and `1e16` is not; `1e12f32` is fixed and `1e13f32` is
/// not), and `tests/oracle.rs` re-checks them over tens of thousands of random
/// bit patterns in each width.
fn write_layout(
    out: &mut Vec<u8>,
    negative: bool,
    digits: &str,
    exp10: i32,
    fixed_range: core::ops::RangeInclusive<i32>,
) {
    let kk = exp10.saturating_add(1);
    let n = digits.len() as i32;
    if negative {
        out.push(b'-');
    }
    if fixed_range.contains(&kk) {
        if kk <= 0 {
            out.extend_from_slice(b"0.");
            for _ in 0..(-kk) {
                out.push(b'0');
            }
            out.extend_from_slice(digits.as_bytes());
        } else if kk >= n {
            out.extend_from_slice(digits.as_bytes());
            for _ in 0..(kk.saturating_sub(n)) {
                out.push(b'0');
            }
            out.extend_from_slice(b".0");
        } else {
            // `0 < kk < n`, so both halves are non-empty.
            let split = kk.max(0) as usize;
            let (head, tail) = digits.split_at(split.min(digits.len()));
            out.extend_from_slice(head.as_bytes());
            out.push(b'.');
            out.extend_from_slice(tail.as_bytes());
        }
    } else {
        let (head, tail) = digits.split_at(1.min(digits.len()));
        out.extend_from_slice(head.as_bytes());
        if !tail.is_empty() {
            out.push(b'.');
            out.extend_from_slice(tail.as_bytes());
        }
        out.push(b'e');
        out.push(if exp10 < 0 { b'-' } else { b'+' });
        write_u128(out, i128::from(exp10).unsigned_abs());
    }
}

/// Write a non-finite or zero float, or report that neither applies.
fn write_special(out: &mut Vec<u8>, finite: bool, zero: bool, negative: bool) -> bool {
    if !finite {
        // JSON has no NaN and no infinity.
        out.extend_from_slice(b"null");
        return true;
    }
    if zero {
        // Preserves the sign of negative zero, as `serde_json` does.
        out.extend_from_slice(if negative { b"-0.0" } else { b"0.0" });
        return true;
    }
    false
}

/// Write an `f64` exactly as `serde_json` writes one.
pub(crate) fn write_f64(out: &mut Vec<u8>, value: f64) {
    if write_special(
        out,
        value.is_finite(),
        value == 0.0,
        value.is_sign_negative(),
    ) {
        return;
    }
    let (digits, exp10) = shortest_f64(value);
    write_layout(out, value.is_sign_negative(), &digits, exp10, -4..=16);
}

/// Write an `f32` exactly as `serde_json` writes one.
pub(crate) fn write_f32(out: &mut Vec<u8>, value: f32) {
    if write_special(
        out,
        value.is_finite(),
        value == 0.0,
        value.is_sign_negative(),
    ) {
        return;
    }
    let (digits, exp10) = shortest_f32(value);
    write_layout(out, value.is_sign_negative(), &digits, exp10, -5..=13);
}

/// The compact JSON serializer.
pub(crate) struct Serializer<'a> {
    pub(crate) out: &'a mut Vec<u8>,
}

impl<'a> ser::Serializer for Serializer<'a> {
    type Ok = ();
    type Error = Error;
    type SerializeSeq = Compound<'a>;
    type SerializeTuple = Compound<'a>;
    type SerializeTupleStruct = Compound<'a>;
    type SerializeTupleVariant = Compound<'a>;
    type SerializeMap = Compound<'a>;
    type SerializeStruct = Compound<'a>;
    type SerializeStructVariant = Compound<'a>;

    fn serialize_bool(self, v: bool) -> Result<()> {
        self.out
            .extend_from_slice(if v { b"true" } else { b"false" });
        Ok(())
    }

    fn serialize_i8(self, v: i8) -> Result<()> {
        self.serialize_i128(i128::from(v))
    }
    fn serialize_i16(self, v: i16) -> Result<()> {
        self.serialize_i128(i128::from(v))
    }
    fn serialize_i32(self, v: i32) -> Result<()> {
        self.serialize_i128(i128::from(v))
    }
    fn serialize_i64(self, v: i64) -> Result<()> {
        self.serialize_i128(i128::from(v))
    }
    fn serialize_i128(self, v: i128) -> Result<()> {
        write_i128(self.out, v);
        Ok(())
    }
    fn serialize_u8(self, v: u8) -> Result<()> {
        self.serialize_u128(u128::from(v))
    }
    fn serialize_u16(self, v: u16) -> Result<()> {
        self.serialize_u128(u128::from(v))
    }
    fn serialize_u32(self, v: u32) -> Result<()> {
        self.serialize_u128(u128::from(v))
    }
    fn serialize_u64(self, v: u64) -> Result<()> {
        self.serialize_u128(u128::from(v))
    }
    fn serialize_u128(self, v: u128) -> Result<()> {
        write_u128(self.out, v);
        Ok(())
    }

    fn serialize_f32(self, v: f32) -> Result<()> {
        write_f32(self.out, v);
        Ok(())
    }

    fn serialize_f64(self, v: f64) -> Result<()> {
        write_f64(self.out, v);
        Ok(())
    }

    fn serialize_char(self, v: char) -> Result<()> {
        let mut buf = [0u8; 4];
        write_json_string(self.out, v.encode_utf8(&mut buf));
        Ok(())
    }

    fn serialize_str(self, v: &str) -> Result<()> {
        write_json_string(self.out, v);
        Ok(())
    }

    /// Bytes become an array of numbers — JSON has no byte string, and this is
    /// what `serde_json` does with `serialize_bytes`.
    fn serialize_bytes(self, v: &[u8]) -> Result<()> {
        self.out.push(b'[');
        for (i, &byte) in v.iter().enumerate() {
            if i > 0 {
                self.out.push(b',');
            }
            write_u128(self.out, u128::from(byte));
        }
        self.out.push(b']');
        Ok(())
    }

    fn serialize_none(self) -> Result<()> {
        self.serialize_unit()
    }

    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<()> {
        value.serialize(self)
    }

    fn serialize_unit(self) -> Result<()> {
        self.out.extend_from_slice(b"null");
        Ok(())
    }

    fn serialize_unit_struct(self, _name: &'static str) -> Result<()> {
        self.serialize_unit()
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
    ) -> Result<()> {
        self.serialize_str(variant)
    }

    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<()> {
        value.serialize(self)
    }

    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<()> {
        self.out.push(b'{');
        write_json_string(self.out, variant);
        self.out.push(b':');
        value.serialize(Serializer { out: self.out })?;
        self.out.push(b'}');
        Ok(())
    }

    fn serialize_seq(self, _len: Option<usize>) -> Result<Compound<'a>> {
        self.out.push(b'[');
        Ok(Compound {
            out: self.out,
            first: true,
            close: Close::Array,
        })
    }

    fn serialize_tuple(self, len: usize) -> Result<Compound<'a>> {
        self.serialize_seq(Some(len))
    }

    fn serialize_tuple_struct(self, _name: &'static str, len: usize) -> Result<Compound<'a>> {
        self.serialize_seq(Some(len))
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
        _len: usize,
    ) -> Result<Compound<'a>> {
        self.out.push(b'{');
        write_json_string(self.out, variant);
        self.out.extend_from_slice(b":[");
        Ok(Compound {
            out: self.out,
            first: true,
            close: Close::VariantArray,
        })
    }

    fn serialize_map(self, _len: Option<usize>) -> Result<Compound<'a>> {
        self.out.push(b'{');
        Ok(Compound {
            out: self.out,
            first: true,
            close: Close::Object,
        })
    }

    fn serialize_struct(self, _name: &'static str, len: usize) -> Result<Compound<'a>> {
        self.serialize_map(Some(len))
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
        _len: usize,
    ) -> Result<Compound<'a>> {
        self.out.push(b'{');
        write_json_string(self.out, variant);
        self.out.extend_from_slice(b":{");
        Ok(Compound {
            out: self.out,
            first: true,
            close: Close::VariantObject,
        })
    }
}

/// How a compound value closes.
pub(crate) enum Close {
    Array,
    Object,
    VariantArray,
    VariantObject,
}

/// The in-progress array/object writer shared by every compound `serde` trait.
pub(crate) struct Compound<'a> {
    out: &'a mut Vec<u8>,
    first: bool,
    close: Close,
}

impl Compound<'_> {
    fn comma(&mut self) {
        if self.first {
            self.first = false;
        } else {
            self.out.push(b',');
        }
    }

    fn finish(self) -> Result<()> {
        match self.close {
            Close::Array => self.out.push(b']'),
            Close::Object => self.out.push(b'}'),
            Close::VariantArray => self.out.extend_from_slice(b"]}"),
            Close::VariantObject => self.out.extend_from_slice(b"}}"),
        }
        Ok(())
    }

    fn entry<T: Serialize + ?Sized>(&mut self, key: &str, value: &T) -> Result<()> {
        self.comma();
        write_json_string(self.out, key);
        self.out.push(b':');
        value.serialize(Serializer { out: self.out })
    }
}

impl ser::SerializeSeq for Compound<'_> {
    type Ok = ();
    type Error = Error;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<()> {
        self.comma();
        value.serialize(Serializer { out: self.out })
    }
    fn end(self) -> Result<()> {
        self.finish()
    }
}

impl ser::SerializeTuple for Compound<'_> {
    type Ok = ();
    type Error = Error;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<()> {
        ser::SerializeSeq::serialize_element(self, value)
    }
    fn end(self) -> Result<()> {
        self.finish()
    }
}

impl ser::SerializeTupleStruct for Compound<'_> {
    type Ok = ();
    type Error = Error;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<()> {
        ser::SerializeSeq::serialize_element(self, value)
    }
    fn end(self) -> Result<()> {
        self.finish()
    }
}

impl ser::SerializeTupleVariant for Compound<'_> {
    type Ok = ();
    type Error = Error;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<()> {
        ser::SerializeSeq::serialize_element(self, value)
    }
    fn end(self) -> Result<()> {
        self.finish()
    }
}

impl ser::SerializeMap for Compound<'_> {
    type Ok = ();
    type Error = Error;

    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<()> {
        self.comma();
        // A JSON object key is a string. Integers and chars are rendered AS
        // strings (what `serde_json` does, so a `HashMap<u32, _>` still
        // serializes); anything else is a type error rather than a document
        // that no parser could read back.
        key.serialize(KeySerializer { out: self.out })
    }

    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<()> {
        self.out.push(b':');
        value.serialize(Serializer { out: self.out })
    }

    fn end(self) -> Result<()> {
        self.finish()
    }
}

impl ser::SerializeStruct for Compound<'_> {
    type Ok = ();
    type Error = Error;
    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<()> {
        self.entry(key, value)
    }
    fn end(self) -> Result<()> {
        self.finish()
    }
}

impl ser::SerializeStructVariant for Compound<'_> {
    type Ok = ();
    type Error = Error;
    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<()> {
        self.entry(key, value)
    }
    fn end(self) -> Result<()> {
        self.finish()
    }
}

/// Serializer for object KEYS: string-shaped values only.
struct KeySerializer<'a> {
    out: &'a mut Vec<u8>,
}

impl KeySerializer<'_> {
    fn quoted_number(self, write: impl FnOnce(&mut Vec<u8>)) -> Result<()> {
        self.out.push(b'"');
        write(self.out);
        self.out.push(b'"');
        Ok(())
    }
}

fn key_type_error() -> Error {
    Error::message("JSON object keys must be strings")
}

impl ser::Serializer for KeySerializer<'_> {
    type Ok = ();
    type Error = Error;
    type SerializeSeq = ser::Impossible<(), Error>;
    type SerializeTuple = ser::Impossible<(), Error>;
    type SerializeTupleStruct = ser::Impossible<(), Error>;
    type SerializeTupleVariant = ser::Impossible<(), Error>;
    type SerializeMap = ser::Impossible<(), Error>;
    type SerializeStruct = ser::Impossible<(), Error>;
    type SerializeStructVariant = ser::Impossible<(), Error>;

    fn serialize_str(self, v: &str) -> Result<()> {
        write_json_string(self.out, v);
        Ok(())
    }

    fn serialize_char(self, v: char) -> Result<()> {
        let mut buf = [0u8; 4];
        write_json_string(self.out, v.encode_utf8(&mut buf));
        Ok(())
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
    ) -> Result<()> {
        self.serialize_str(variant)
    }

    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<()> {
        value.serialize(self)
    }

    fn serialize_i8(self, v: i8) -> Result<()> {
        self.serialize_i128(i128::from(v))
    }
    fn serialize_i16(self, v: i16) -> Result<()> {
        self.serialize_i128(i128::from(v))
    }
    fn serialize_i32(self, v: i32) -> Result<()> {
        self.serialize_i128(i128::from(v))
    }
    fn serialize_i64(self, v: i64) -> Result<()> {
        self.serialize_i128(i128::from(v))
    }
    fn serialize_i128(self, v: i128) -> Result<()> {
        self.quoted_number(|out| write_i128(out, v))
    }
    fn serialize_u8(self, v: u8) -> Result<()> {
        self.serialize_u128(u128::from(v))
    }
    fn serialize_u16(self, v: u16) -> Result<()> {
        self.serialize_u128(u128::from(v))
    }
    fn serialize_u32(self, v: u32) -> Result<()> {
        self.serialize_u128(u128::from(v))
    }
    fn serialize_u64(self, v: u64) -> Result<()> {
        self.serialize_u128(u128::from(v))
    }
    fn serialize_u128(self, v: u128) -> Result<()> {
        self.quoted_number(|out| write_u128(out, v))
    }
    fn serialize_bool(self, _v: bool) -> Result<()> {
        Err(key_type_error())
    }
    fn serialize_f32(self, _v: f32) -> Result<()> {
        Err(key_type_error())
    }
    fn serialize_f64(self, _v: f64) -> Result<()> {
        Err(key_type_error())
    }
    fn serialize_bytes(self, _v: &[u8]) -> Result<()> {
        Err(key_type_error())
    }
    fn serialize_none(self) -> Result<()> {
        Err(key_type_error())
    }
    fn serialize_some<T: Serialize + ?Sized>(self, _value: &T) -> Result<()> {
        Err(key_type_error())
    }
    fn serialize_unit(self) -> Result<()> {
        Err(key_type_error())
    }
    fn serialize_unit_struct(self, _name: &'static str) -> Result<()> {
        Err(key_type_error())
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        _value: &T,
    ) -> Result<()> {
        Err(key_type_error())
    }
    fn serialize_seq(self, _len: Option<usize>) -> Result<Self::SerializeSeq> {
        Err(key_type_error())
    }
    fn serialize_tuple(self, _len: usize) -> Result<Self::SerializeTuple> {
        Err(key_type_error())
    }
    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleStruct> {
        Err(key_type_error())
    }
    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleVariant> {
        Err(key_type_error())
    }
    fn serialize_map(self, _len: Option<usize>) -> Result<Self::SerializeMap> {
        Err(key_type_error())
    }
    fn serialize_struct(self, _name: &'static str, _len: usize) -> Result<Self::SerializeStruct> {
        Err(key_type_error())
    }
    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStructVariant> {
        Err(key_type_error())
    }
}
