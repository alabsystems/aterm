// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! D-Bus wire marshalling and bounds-checked decoding with container depth and size limits.

use crate::error::{Error, Result};
use crate::message::MAX_MESSAGE;
use crate::names;
use crate::sig::{self, SigFault, Type};
use crate::value::{Array, Dict, ObjectPath, Signature, Value};

/// The longest array the specification allows, in bytes.
pub const MAX_ARRAY_BYTES: usize = 1 << 26;
/// How deep containers may nest in one message, variants included.
pub const MAX_DEPTH: usize = 64;

/// A message's byte order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Endian {
    /// `l`
    Little,
    /// `B`
    Big,
}

impl Endian {
    /// This machine's order, which every message this side writes uses.
    pub const NATIVE: Self = if cfg!(target_endian = "big") {
        Self::Big
    } else {
        Self::Little
    };

    /// The header's first byte.
    #[inline]
    #[must_use]
    pub const fn tag(self) -> u8 {
        match self {
            Self::Little => b'l',
            Self::Big => b'B',
        }
    }

    #[inline]
    #[must_use]
    pub const fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            b'l' => Some(Self::Little),
            b'B' => Some(Self::Big),
            _ => None,
        }
    }
}

/// Return the first offset at or after `pos` that is a multiple of `align`. Not `const`: Trust
/// 0.11.0 refuses a contract on a `const fn`.
#[cfg_attr(trust_verify, trust::requires(align == 1 || align == 2 || align == 4 || align == 8))]
#[cfg_attr(trust_verify, trust::requires(pos <= 4294967296))]
#[cfg_attr(trust_verify, trust::ensures(result >= pos && result - pos < align && result % align == 0))]
#[inline]
#[must_use]
pub fn pad_to(pos: usize, align: usize) -> usize {
    let rem = pos % align;
    if rem == 0 { pos } else { pos + (align - rem) }
}

/// Encode `values` back to back starting at message offset `offset`.
/// # Errors
/// [`Error::Invalid`] for a value the specification forbids, among them values whose types no
/// signature can carry (see [`decode`]) and an offset past the longest message.
pub fn encode(values: &[Value], endian: Endian, offset: usize) -> Result<Vec<u8>> {
    check_types(
        &values.iter().map(Value::type_of).collect::<Vec<_>>(),
        offset,
    )?;
    encode_checked(values, endian, offset)
}

/// [`encode`] of values whose signature the caller has already parsed.
pub(crate) fn encode_checked(values: &[Value], endian: Endian, offset: usize) -> Result<Vec<u8>> {
    let mut w = Writer::new(endian, offset);
    for value in values {
        w.value(value)?;
    }
    Ok(w.buf)
}

/// Decode `bytes` as the sequence `types` starting at message offset `offset`.
/// # Errors
/// [`Error::Invalid`] for `types` no signature can carry (an empty struct, a dict keyed by a
/// container, past the length or nesting limits of [`sig`]) or an offset past the longest message;
/// [`Error::Malformed`] unless `bytes` hold exactly one value of each type; [`Error::Unsupported`]
/// for a variant holding a unix descriptor.
pub fn decode(bytes: &[u8], types: &[Type], endian: Endian, offset: usize) -> Result<Vec<Value>> {
    check_types(types, offset)?;
    decode_checked(bytes, types, endian, offset)
}

/// Holds caller-built `types` to what a signature on the wire can say ([`Type`]).
fn check_types(types: &[Type], offset: usize) -> Result<()> {
    if offset > MAX_MESSAGE {
        return Err(Error::Invalid("offset past the longest message"));
    }
    sig::parse_many(sig::signature_of(types).as_bytes())
        .map(drop)
        .map_err(SigFault::refused)
}

/// [`decode`] under types the signature parser produced.
pub(crate) fn decode_checked(
    bytes: &[u8],
    types: &[Type],
    endian: Endian,
    offset: usize,
) -> Result<Vec<Value>> {
    let mut r = Reader::new(bytes, endian, offset);
    let values = types
        .iter()
        .map(|ty| r.value(ty))
        .collect::<Result<Vec<_>>>()?;
    if r.pos != bytes.len() {
        return Err(Error::malformed("bytes left after the last value", r.at()));
    }
    Ok(values)
}

/// Writes values into a buffer whose first byte sits at message offset `base`.
pub(crate) struct Writer {
    pub(crate) buf: Vec<u8>,
    endian: Endian,
    base: usize,
    depth: usize,
}

impl Writer {
    #[inline]
    pub(crate) const fn new(endian: Endian, base: usize) -> Self {
        Self {
            buf: Vec::new(),
            endian,
            base,
            depth: 0,
        }
    }

    #[inline]
    const fn pos(&self) -> usize {
        self.base.saturating_add(self.buf.len())
    }

    #[inline]
    pub(crate) fn align(&mut self, align: usize) {
        let pos = self.pos();
        let pad = pad_to(pos, align).saturating_sub(pos);
        self.buf.resize(self.buf.len().saturating_add(pad), 0);
    }

    #[inline]
    fn u16(&mut self, v: u16) {
        self.align(2);
        match self.endian {
            Endian::Little => self.buf.extend_from_slice(&v.to_le_bytes()),
            Endian::Big => self.buf.extend_from_slice(&v.to_be_bytes()),
        }
    }

    #[inline]
    pub(crate) fn u32(&mut self, v: u32) {
        self.align(4);
        match self.endian {
            Endian::Little => self.buf.extend_from_slice(&v.to_le_bytes()),
            Endian::Big => self.buf.extend_from_slice(&v.to_be_bytes()),
        }
    }

    #[inline]
    fn u64(&mut self, v: u64) {
        self.align(8);
        match self.endian {
            Endian::Little => self.buf.extend_from_slice(&v.to_le_bytes()),
            Endian::Big => self.buf.extend_from_slice(&v.to_be_bytes()),
        }
    }

    /// Overwrites the `u32` at buffer index `at` (an array's length slot).
    #[inline]
    pub(crate) fn patch_u32(&mut self, at: usize, v: u32) {
        let bytes = match self.endian {
            Endian::Little => v.to_le_bytes(),
            Endian::Big => v.to_be_bytes(),
        };
        if let Some(slot) = self.buf.get_mut(at..at.saturating_add(4)) {
            slot.copy_from_slice(&bytes);
        }
    }

    fn string(&mut self, s: &str) -> Result<()> {
        if s.as_bytes().contains(&0) {
            return Err(Error::Invalid("NUL inside a string"));
        }
        let len = u32::try_from(s.len()).map_err(|_| Error::Invalid("string longer than 4 GiB"))?;
        self.u32(len);
        self.buf.extend_from_slice(s.as_bytes());
        self.buf.push(0);
        Ok(())
    }

    pub(crate) fn signature(&mut self, sig: &str) -> Result<()> {
        let len = u8::try_from(sig.len())
            .map_err(|_| Error::Invalid("signature longer than 255 bytes"))?;
        self.buf.push(len);
        self.buf.extend_from_slice(sig.as_bytes());
        self.buf.push(0);
        Ok(())
    }

    #[inline]
    const fn enter(&mut self) -> Result<()> {
        if self.depth >= MAX_DEPTH {
            return Err(Error::Invalid("containers nested deeper than 64"));
        }
        self.depth += 1;
        Ok(())
    }

    #[inline]
    const fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    fn array(
        &mut self,
        elem_align: usize,
        each: impl FnOnce(&mut Self) -> Result<()>,
    ) -> Result<()> {
        self.enter()?;
        self.align(4);
        let slot = self.buf.len();
        self.u32(0);
        self.align(elem_align);
        let start = self.buf.len();
        each(self)?;
        let len = self.buf.len().saturating_sub(start);
        if len > MAX_ARRAY_BYTES {
            return Err(Error::Invalid("array longer than 64 MiB"));
        }
        self.patch_u32(slot, len as u32);
        self.leave();
        Ok(())
    }

    pub(crate) fn value(&mut self, value: &Value) -> Result<()> {
        match value {
            Value::Byte(v) => self.buf.push(*v),
            Value::Bool(v) => self.u32(u32::from(*v)),
            Value::I16(v) => self.u16(*v as u16),
            Value::U16(v) => self.u16(*v),
            Value::I32(v) => self.u32(*v as u32),
            Value::U32(v) => self.u32(*v),
            Value::I64(v) => self.u64(*v as u64),
            Value::U64(v) => self.u64(*v),
            Value::F64(v) => self.u64(v.to_bits()),
            Value::Str(s) => self.string(s)?,
            Value::Path(p) => self.string(p.as_str())?,
            Value::Sig(s) => self.signature(s.as_str())?,
            Value::Variant(inner) => {
                self.enter()?;
                let sig = inner.type_of().signature();
                sig::parse_one(sig.as_bytes()).map_err(sig::SigFault::refused)?;
                self.signature(&sig)?;
                self.value(inner)?;
                self.leave();
            }
            Value::Array(a) => {
                self.array(a.elem().alignment(), |w| {
                    a.items().iter().try_for_each(|item| w.value(item))
                })?;
            }
            Value::Dict(d) => {
                self.array(8, |w| {
                    d.entries().iter().try_for_each(|(k, v)| {
                        w.enter()?;
                        w.align(8);
                        w.value(k)?;
                        w.value(v)?;
                        w.leave();
                        Ok(())
                    })
                })?;
            }
            Value::Struct(fields) => {
                if fields.is_empty() {
                    return Err(Error::Invalid("empty struct"));
                }
                self.enter()?;
                self.align(8);
                for field in fields {
                    self.value(field)?;
                }
                self.leave();
            }
        }
        Ok(())
    }
}

/// Reads values from bytes whose first byte sits at message offset `base`.
pub(crate) struct Reader<'a> {
    buf: &'a [u8],
    pub(crate) pos: usize,
    endian: Endian,
    base: usize,
    depth: usize,
}

impl<'a> Reader<'a> {
    #[inline]
    pub(crate) const fn new(buf: &'a [u8], endian: Endian, base: usize) -> Self {
        Self {
            buf,
            pos: 0,
            endian,
            base,
            depth: 0,
        }
    }

    /// The message offset of the next byte.
    #[inline]
    pub(crate) const fn at(&self) -> usize {
        self.base.saturating_add(self.pos)
    }

    #[inline]
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let at = self.at();
        let end = self
            .pos
            .checked_add(n)
            .ok_or(Error::malformed("length overflows", at))?;
        let bytes = self
            .buf
            .get(self.pos..end)
            .ok_or(Error::malformed("value runs past the end", at))?;
        self.pos = end;
        Ok(bytes)
    }

    #[inline]
    pub(crate) fn align(&mut self, align: usize) -> Result<()> {
        let at = self.at();
        if at > MAX_MESSAGE {
            return Err(Error::malformed("offset past the longest message", at));
        }
        let pad = self.take(pad_to(at, align).saturating_sub(at))?;
        if pad.iter().any(|&b| b != 0) {
            return Err(Error::malformed("nonzero padding", at));
        }
        Ok(())
    }

    #[inline]
    pub(crate) fn u8(&mut self) -> Result<u8> {
        let b = self.take(1)?;
        Ok(b.first().copied().unwrap_or(0))
    }

    #[inline]
    fn fixed<const N: usize>(&mut self) -> Result<[u8; N]> {
        self.align(N)?;
        let mut out = [0u8; N];
        out.copy_from_slice(self.take(N)?);
        if self.endian == Endian::Big {
            out.reverse();
        }
        Ok(out)
    }

    #[inline]
    fn u16(&mut self) -> Result<u16> {
        self.fixed::<2>().map(u16::from_le_bytes)
    }

    #[inline]
    pub(crate) fn u32(&mut self) -> Result<u32> {
        self.fixed::<4>().map(u32::from_le_bytes)
    }

    #[inline]
    fn u64(&mut self) -> Result<u64> {
        self.fixed::<8>().map(u64::from_le_bytes)
    }

    fn string(&mut self) -> Result<String> {
        let at = self.at();
        let len = self.u32()? as usize;
        let bytes = self.take(len)?;
        if self.u8()? != 0 {
            return Err(Error::malformed("string not NUL-terminated", at));
        }
        if bytes.contains(&0) {
            return Err(Error::malformed("NUL inside a string", at));
        }
        String::from_utf8(bytes.to_vec()).map_err(|_| Error::malformed("string is not UTF-8", at))
    }

    /// A signature's raw bytes (checked by the caller's parse).
    pub(crate) fn signature_bytes(&mut self) -> Result<&'a [u8]> {
        let at = self.at();
        let len = usize::from(self.u8()?);
        let bytes = self.take(len)?;
        if self.u8()? != 0 {
            return Err(Error::malformed("signature not NUL-terminated", at));
        }
        Ok(bytes)
    }

    #[inline]
    const fn enter(&mut self) -> Result<()> {
        if self.depth >= MAX_DEPTH {
            return Err(Error::malformed(
                "containers nested deeper than 64",
                self.at(),
            ));
        }
        self.depth += 1;
        Ok(())
    }

    #[inline]
    const fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    /// Read an array's byte length, padding, and elements until that byte length is consumed.
    pub(crate) fn array<T>(
        &mut self,
        elem_align: usize,
        mut each: impl FnMut(&mut Self) -> Result<T>,
    ) -> Result<Vec<T>> {
        let at = self.at();
        let len = self.u32()? as usize;
        if len > MAX_ARRAY_BYTES {
            return Err(Error::malformed("array longer than 64 MiB", at));
        }
        self.align(elem_align)?;
        let end = self
            .pos
            .checked_add(len)
            .filter(|&end| end <= self.buf.len());
        let end = end.ok_or(Error::malformed("array runs past the end", at))?;
        let whole = self.buf;
        self.buf = whole.get(..end).unwrap_or(whole);
        let mut items = Vec::new();
        let mut result = Ok(());
        while self.pos < end {
            let before = self.pos;
            match each(self) {
                Ok(item) if self.pos > before => items.push(item),
                Ok(_) => {
                    result = Err(Error::malformed("array element of no bytes", at));
                    break;
                }
                Err(e) => {
                    result = Err(e);
                    break;
                }
            }
        }
        self.buf = whole;
        result.map(|()| items)
    }

    pub(crate) fn value(&mut self, ty: &Type) -> Result<Value> {
        let at = self.at();
        Ok(match ty {
            Type::Byte => Value::Byte(self.u8()?),
            Type::Bool => match self.u32()? {
                0 => Value::Bool(false),
                1 => Value::Bool(true),
                _ => return Err(Error::malformed("boolean other than 0 or 1", at)),
            },
            Type::I16 => Value::I16(self.u16()? as i16),
            Type::U16 => Value::U16(self.u16()?),
            Type::I32 => Value::I32(self.u32()? as i32),
            Type::U32 => Value::U32(self.u32()?),
            Type::I64 => Value::I64(self.u64()? as i64),
            Type::U64 => Value::U64(self.u64()?),
            Type::F64 => Value::F64(f64::from_bits(self.u64()?)),
            Type::Str => Value::Str(self.string()?),
            Type::Path => {
                let path = self.string()?;
                if !names::is_object_path(&path) {
                    return Err(Error::malformed("invalid object path", at));
                }
                Value::Path(ObjectPath::checked(path))
            }
            Type::Sig => {
                let bytes = self.signature_bytes()?;
                sig::check_data(bytes).map_err(|f| f.on_wire(at))?;
                let text = std::str::from_utf8(bytes)
                    .map_err(|_| Error::malformed("signature is not ASCII", at))?;
                Value::Sig(Signature::checked(text.to_owned()))
            }
            Type::Variant => {
                self.enter()?;
                let inner = sig::parse_one(self.signature_bytes()?).map_err(|f| f.on_wire(at))?;
                let value = self.value(&inner)?;
                self.leave();
                Value::variant(value)
            }
            Type::Array(elem) => {
                self.enter()?;
                let items = self.array(elem.alignment(), |r| r.value(elem))?;
                self.leave();
                Value::Array(Box::new(Array::checked((**elem).clone(), items)))
            }
            Type::Dict(key, value) => {
                self.enter()?;
                let entries = self.array(8, |r| {
                    r.enter()?;
                    r.align(8)?;
                    let k = r.value(key)?;
                    let v = r.value(value)?;
                    r.leave();
                    Ok((k, v))
                })?;
                self.leave();
                Value::Dict(Box::new(Dict::checked(
                    (**key).clone(),
                    (**value).clone(),
                    entries,
                )))
            }
            Type::Struct(fields) => {
                if fields.is_empty() {
                    return Err(Error::Invalid("empty struct"));
                }
                self.enter()?;
                self.align(8)?;
                let values = fields
                    .iter()
                    .map(|f| self.value(f))
                    .collect::<Result<Vec<_>>>()?;
                self.leave();
                Value::Struct(values)
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn de(hex: &str, sig: &str) -> Result<Vec<Value>> {
        let bytes = aterm_codec::hex::decode_vec(hex.as_bytes()).unwrap();
        decode(
            &bytes,
            &sig::parse_signature(sig).unwrap(),
            Endian::Little,
            0,
        )
    }

    fn malformed(r: Result<Vec<Value>>) -> &'static str {
        match r {
            Err(Error::Malformed { what, .. }) => what,
            other => panic!("expected a malformed refusal, got {other:?}"),
        }
    }

    #[test]
    fn pad_to_lands_on_the_next_boundary() {
        for align in [1, 2, 4, 8] {
            for pos in 0..32 {
                let p = pad_to(pos, align);
                assert!(p >= pos && p - pos < align && p.is_multiple_of(align));
            }
        }
    }

    #[test]
    fn the_reader_refuses_what_the_specification_forbids() {
        assert_eq!(malformed(de("02000000", "b")), "boolean other than 0 or 1");
        assert_eq!(malformed(de("0100000041", "s")), "value runs past the end");
        assert_eq!(
            malformed(de("010000004101", "s")),
            "string not NUL-terminated"
        );
        assert_eq!(
            malformed(de("020000004100", "s")),
            "value runs past the end"
        );
        assert_eq!(malformed(de("02000000410000", "s")), "NUL inside a string");
        assert_eq!(malformed(de("01000000ff00", "s")), "string is not UTF-8");
        assert_eq!(malformed(de("02000000612f00", "o")), "invalid object path");
        assert_eq!(
            malformed(de("01280000", "g")),
            "signature ends inside a type"
        );
        assert_eq!(malformed(de("0101000000", "yu")), "nonzero padding");
        assert_eq!(
            malformed(de("0000000001", "u")),
            "bytes left after the last value"
        );
        assert_eq!(malformed(de("ffffff7f", "ay")), "array longer than 64 MiB");
        assert_eq!(
            malformed(de("05000000010203", "ay")),
            "array runs past the end"
        );
        assert_eq!(
            malformed(de("02000000010000000200000000000000", "au")),
            "value runs past the end"
        );
        assert!(matches!(
            de("0168000000000000", "v"),
            Err(Error::Unsupported(_))
        ));
        assert_eq!(
            de("0000000000000000", "a{sv}").unwrap()[0]
                .as_dict()
                .map(|d| d.entries().len()),
            Some(0)
        );
        assert_eq!(
            malformed(de("00000000", "a{sv}")),
            "value runs past the end"
        );
        assert_eq!(
            malformed(de("0000000001000000", "a{sv}")),
            "nonzero padding"
        );
    }

    #[test]
    fn nesting_is_bounded_across_variants() {
        let nested = |n: usize| {
            let mut v = Value::Byte(7);
            for _ in 0..n {
                v = Value::variant(v);
            }
            v
        };
        let bytes = encode(&[nested(64)], Endian::Little, 0).unwrap();
        assert!(decode(&bytes, &[Type::Variant], Endian::Little, 0).is_ok());
        assert!(matches!(
            encode(&[nested(65)], Endian::Little, 0),
            Err(Error::Invalid(_))
        ));
        let mut deep = Vec::new();
        for _ in 0..65 {
            deep.extend_from_slice(b"\x01v\x00");
        }
        deep.extend_from_slice(b"\x01y\x00\x07");
        assert_eq!(
            malformed(decode(&deep, &[Type::Variant], Endian::Little, 0)),
            "containers nested deeper than 64"
        );
    }

    #[test]
    fn the_writer_refuses_what_the_specification_forbids() {
        assert!(matches!(
            encode(&[Value::Str("a\0b".into())], Endian::Little, 0),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            encode(&[Value::Struct(vec![])], Endian::Little, 0),
            Err(Error::Invalid(_))
        ));
        let nested = |n: usize| {
            let mut v = Value::Byte(7);
            for _ in 0..n {
                v = Value::array(v.type_of(), vec![v]).unwrap();
            }
            v
        };
        assert!(encode(&[nested(32)], Endian::Little, 0).is_ok());
        assert!(matches!(
            encode(&[nested(33)], Endian::Little, 0),
            Err(Error::Invalid("arrays nested deeper than 32"))
        ));
        assert!(matches!(
            encode(&[Value::Byte(1)], Endian::Little, MAX_MESSAGE + 1),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn caller_types_are_held_to_the_signature_rules() {
        let mut r = Reader::new(&[1, 0, 0, 0, 7], Endian::Little, 0);
        let mut calls = 0;
        let nothing = |_: &mut Reader<'_>| {
            calls += 1;
            if calls > 1 {
                Err(Error::Invalid("looped"))
            } else {
                Ok(())
            }
        };
        match r.array(1, nothing) {
            Err(Error::Malformed { what, .. }) => assert_eq!(what, "array element of no bytes"),
            other => panic!("an element of no bytes: {other:?}"),
        }
        let empty = Type::structure([]);
        let mut r = Reader::new(&[0; 8], Endian::Little, 0);
        assert!(matches!(
            r.value(&empty),
            Err(Error::Invalid("empty struct"))
        ));
        let mut bytes = vec![8, 0, 0, 0];
        bytes.extend_from_slice(&[0; 12]);
        let refused = |types: &[Type]| decode(&bytes, types, Endian::Little, 0).unwrap_err();
        assert!(matches!(
            refused(&[Type::array(empty)]),
            Error::Invalid("empty struct")
        ));
        let one = decode(
            &bytes,
            &[Type::array(Type::structure([Type::U64]))],
            Endian::Little,
            0,
        );
        assert_eq!(one.unwrap()[0].as_array().map(|a| a.items().len()), Some(1));
        assert!(matches!(
            refused(&[Type::dict(Type::Variant, Type::U64)]),
            Error::Invalid("dict key is not a basic type")
        ));
        let mut deep = Type::Byte;
        for _ in 0..33 {
            deep = Type::array(deep);
        }
        assert!(matches!(
            refused(&[deep]),
            Error::Invalid("arrays nested deeper than 32")
        ));
        assert!(matches!(
            decode(&[7], &[Type::Byte], Endian::Little, MAX_MESSAGE + 1),
            Err(Error::Invalid(_))
        ));
        assert!(decode(&[7], &[Type::Byte], Endian::Little, MAX_MESSAGE).is_ok());
    }

    #[test]
    fn a_signature_value_may_name_a_descriptor() {
        for (hex, sig) in [("016800", "h"), ("05617b68737d00", "a{hs}")] {
            assert_eq!(
                de(hex, "g").unwrap(),
                vec![Value::Sig(Signature::new(sig).unwrap())]
            );
        }
        assert_eq!(
            malformed(de("01280000", "g")),
            "signature ends inside a type"
        );
        assert!(matches!(
            de("0168000000000000", "v"),
            Err(Error::Unsupported(_))
        ));
    }

    fn variant_dictionary(variants: usize) -> Value {
        let mut value = Value::dict(
            Type::Byte,
            Type::Byte,
            vec![(Value::Byte(1), Value::Byte(2))],
        )
        .unwrap();
        for _ in 0..variants {
            value = Value::variant(value);
        }
        value
    }

    fn variant_dictionary_bytes(variants: usize) -> Vec<u8> {
        let mut bytes = Vec::new();
        for _ in 1..variants {
            bytes.extend_from_slice(b"\x01v\x00");
        }
        bytes.extend_from_slice(b"\x05a{yy}\x00");
        while bytes.len() % 4 != 0 {
            bytes.push(0);
        }
        bytes.extend_from_slice(&2u32.to_le_bytes());
        while bytes.len() % 8 != 0 {
            bytes.push(0);
        }
        bytes.extend_from_slice(&[1, 2]);
        bytes
    }

    #[test]
    fn dictionary_entries_count_towards_the_encoder_depth_limit() {
        assert_eq!(
            encode(&[variant_dictionary(62)], Endian::Little, 0).unwrap(),
            variant_dictionary_bytes(62)
        );
        assert!(matches!(
            encode(&[variant_dictionary(63)], Endian::Little, 0),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn dictionary_entries_count_towards_the_decoder_depth_limit() {
        assert_eq!(
            decode(
                &variant_dictionary_bytes(62),
                &[Type::Variant],
                Endian::Little,
                0
            )
            .unwrap(),
            vec![variant_dictionary(62)]
        );
        assert!(matches!(
            decode(
                &variant_dictionary_bytes(63),
                &[Type::Variant],
                Endian::Little,
                0
            ),
            Err(Error::Malformed { .. })
        ));
    }
}
