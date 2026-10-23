// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Compact `serde::Serializer` and JSON writer matching `serde_json` byte-for-byte.

use serde::{Serialize, ser};

use crate::error::{Error, Result};

/// Serializes `value` as compact JSON bytes.
///
/// # Errors
/// Returns an error if `value`'s `Serialize` implementation fails or uses a non-string map key.
#[inline]
pub fn to_vec<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(128);
    value.serialize(Serializer { out: &mut out })?;
    Ok(out)
}

/// Serializes `value` as a compact JSON string.
///
/// # Errors
/// As [`to_vec`].
#[inline]
pub fn to_string<T: Serialize + ?Sized>(value: &T) -> Result<String> {
    let bytes = to_vec(value)?;
    String::from_utf8(bytes).map_err(|_| Error::message("serializer produced invalid UTF-8"))
}

/// Writes `text` as a quoted JSON string literal.
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

#[inline]
fn hex_nibble(v: u8) -> u8 {
    // `v <= 15` at every call (both arguments are masked or shifted), so the
    // lookup is in range; `get` keeps that local and the fallback is dead.
    *b"0123456789abcdef".get(v as usize).unwrap_or(&b'0')
}

/// Writes an unsigned integer in decimal.
#[inline]
pub(crate) fn write_u128(out: &mut Vec<u8>, value: u128) {
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

/// Writes a signed integer in decimal.
#[inline]
pub(crate) fn write_i128(out: &mut Vec<u8>, value: i128) {
    if value < 0 {
        out.push(b'-');
    }
    write_u128(out, value.unsigned_abs());
}

/// The shortest decimal digit string that reads back as `value`, and of those
/// the closest, with the power of ten it is scaled by — what `ryu`, which
/// `serde_json` formats with, produces.
fn shortest_f64(value: f64) -> (String, i32) {
    let shortest = format!("{value:e}");
    let precision = split_exp(&shortest).0.len().saturating_sub(1);
    let closest = format!("{value:.precision$e}");
    if closest.parse::<f64>() == Ok(value) {
        split_exp(&closest)
    } else {
        split_exp(&shortest)
    }
}

/// As [`shortest_f64`], at `f32` width: `0.1f32` must print as `0.1`, not as
/// the `f64` it widens to.
fn shortest_f32(value: f32) -> (String, i32) {
    let shortest = format!("{value:e}");
    let precision = split_exp(&shortest).0.len().saturating_sub(1);
    let closest = format!("{value:.precision$e}");
    if closest.parse::<f32>() == Ok(value) {
        split_exp(&closest)
    } else {
        split_exp(&shortest)
    }
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
            let split = kk.max(0) as usize;
            let digits = digits.as_bytes();
            let (head, tail) = digits.split_at(split.min(digits.len()));
            out.extend_from_slice(head);
            out.push(b'.');
            out.extend_from_slice(tail);
        }
    } else {
        let digits_b = digits.as_bytes();
        let (head, tail) = digits_b.split_at(1.min(digits_b.len()));
        out.extend_from_slice(head);
        if !tail.is_empty() {
            out.push(b'.');
            out.extend_from_slice(tail);
        }
        out.push(b'e');
        out.push(if exp10 < 0 { b'-' } else { b'+' });
        write_u128(out, i128::from(exp10).unsigned_abs());
    }
}

/// Write a non-finite or zero float, or report that neither applies.
fn write_special(out: &mut Vec<u8>, finite: bool, zero: bool, negative: bool) -> bool {
    if !finite {
        out.extend_from_slice(b"null");
        return true;
    }
    if zero {
        out.extend_from_slice(if negative { b"-0.0" } else { b"0.0" });
        return true;
    }
    false
}

/// Writes an `f64` matching `serde_json` formatting.
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

/// Writes an `f32` matching `serde_json` formatting.
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

/// Compact JSON serializer.
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

/// Closing delimiter for a compound JSON value.
pub(crate) enum Close {
    Array,
    Object,
    VariantArray,
    VariantObject,
}

/// In-progress JSON array or object writer.
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

/// Serializer for object KEYS: strings, and the scalars with one obvious
/// spelling as one (integers, `bool`, `char`, a unit variant's name), through
/// any newtype.
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
    fn serialize_bool(self, v: bool) -> Result<()> {
        self.serialize_str(if v { "true" } else { "false" })
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
