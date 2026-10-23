// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! D-Bus [`Type`] signature parser ([`Error::Unsupported`], [`crate::Signature`]).

use std::sync::Arc;

use crate::error::{Error, Result};

/// The longest signature the wire can carry (its length is one byte).
pub const MAX_SIGNATURE: usize = 255;
/// How deep arrays may nest inside one signature.
pub const MAX_ARRAY_DEPTH: usize = 32;
/// How deep structs and dict entries may nest inside one signature.
pub const MAX_STRUCT_DEPTH: usize = 32;

/// One complete D-Bus type.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Type {
    /// `y`.
    Byte,
    /// `b`, carried as a `u32` that must be 0 or 1.
    Bool,
    /// `n`.
    I16,
    /// `q`.
    U16,
    /// `i`.
    I32,
    /// `u`.
    U32,
    /// `x`.
    I64,
    /// `t`.
    U64,
    /// `d`, an IEEE 754 double.
    F64,
    /// `s`, UTF-8 without NUL.
    Str,
    /// `o`.
    Path,
    /// `g`.
    Sig,
    /// `v`: a signature, then one value of that type.
    Variant,
    /// `a` + element type.
    Array(Arc<Type>),
    /// `a{kv}`: an array of dict entries with a basic key type.
    Dict(Arc<Type>, Arc<Type>),
    /// `(...)`: one or more fields.
    Struct(Arc<[Type]>),
}

impl Type {
    /// `a` + `elem`.
    #[inline]
    #[must_use]
    pub fn array(elem: Type) -> Self {
        Self::Array(Arc::new(elem))
    }

    /// `a{kv}` ([`crate::Value::dict`]).
    #[inline]
    #[must_use]
    pub fn dict(key: Type, value: Type) -> Self {
        Self::Dict(Arc::new(key), Arc::new(value))
    }

    /// `(...)`.
    #[inline]
    #[must_use]
    pub fn structure(fields: impl IntoIterator<Item = Type>) -> Self {
        Self::Struct(fields.into_iter().collect())
    }

    /// The boundary a value of this type starts on.
    #[inline]
    #[must_use]
    pub const fn alignment(&self) -> usize {
        match self {
            Self::Byte | Self::Sig | Self::Variant => 1,
            Self::I16 | Self::U16 => 2,
            Self::Bool | Self::I32 | Self::U32 | Self::Str | Self::Path => 4,
            Self::Array(_) | Self::Dict(..) => 4,
            Self::I64 | Self::U64 | Self::F64 | Self::Struct(_) => 8,
        }
    }

    /// Whether this type may key a dict entry.
    #[inline]
    #[must_use]
    pub const fn is_basic(&self) -> bool {
        !matches!(
            self,
            Self::Variant | Self::Array(_) | Self::Dict(..) | Self::Struct(_)
        )
    }

    /// Appends this type's signature.
    pub fn push_signature(&self, out: &mut String) {
        let code = match self {
            Self::Byte => 'y',
            Self::Bool => 'b',
            Self::I16 => 'n',
            Self::U16 => 'q',
            Self::I32 => 'i',
            Self::U32 => 'u',
            Self::I64 => 'x',
            Self::U64 => 't',
            Self::F64 => 'd',
            Self::Str => 's',
            Self::Path => 'o',
            Self::Sig => 'g',
            Self::Variant => 'v',
            Self::Array(elem) => {
                out.push('a');
                elem.push_signature(out);
                return;
            }
            Self::Dict(key, value) => {
                out.push_str("a{");
                key.push_signature(out);
                value.push_signature(out);
                out.push('}');
                return;
            }
            Self::Struct(fields) => {
                out.push('(');
                for field in fields.iter() {
                    field.push_signature(out);
                }
                out.push(')');
                return;
            }
        };
        out.push(code);
    }

    /// This type's signature.
    #[inline]
    #[must_use]
    pub fn signature(&self) -> String {
        let mut out = String::new();
        self.push_signature(&mut out);
        out
    }

    /// Parses a signature holding exactly one complete type (a variant's).
    ///
    /// # Errors
    /// [`Error::Invalid`] if forbidden, or [`Error::Unsupported`] for a unix descriptor.
    #[inline]
    pub fn parse(sig: &str) -> Result<Self> {
        parse_one(sig.as_bytes()).map_err(SigFault::refused)
    }
}

/// The signature of a sequence of types (a message body's).
#[must_use]
pub fn signature_of(types: &[Type]) -> String {
    let mut out = String::new();
    for ty in types {
        ty.push_signature(&mut out);
    }
    out
}

/// Parses a signature holding zero or more complete types (a message body's).
///
/// # Errors
/// As [`Type::parse`].
#[inline]
pub fn parse_signature(sig: &str) -> Result<Vec<Type>> {
    parse_many(sig.as_bytes()).map_err(SigFault::refused)
}

/// Why the parser refused a signature ([`Error::Invalid`], [`Error::Malformed`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SigFault {
    Bad(&'static str),
    Fd,
}

impl SigFault {
    #[inline]
    pub(crate) const fn refused(self) -> Error {
        match self {
            Self::Bad(what) => Error::Invalid(what),
            Self::Fd => Error::Unsupported("unix descriptors (type h)"),
        }
    }

    #[inline]
    pub(crate) const fn on_wire(self, at: usize) -> Error {
        match self {
            Self::Bad(what) => Error::malformed(what, at),
            Self::Fd => Error::Unsupported("unix descriptors (type h)"),
        }
    }
}

pub(crate) fn parse_many(sig: &[u8]) -> std::result::Result<Vec<Type>, SigFault> {
    let mut parser = Parser::new(sig, false)?;
    let mut types = Vec::new();
    while parser.pos < sig.len() {
        types.push(parser.complete()?);
    }
    Ok(types)
}

pub(crate) fn parse_one(sig: &[u8]) -> std::result::Result<Type, SigFault> {
    let mut parser = Parser::new(sig, false)?;
    let ty = parser.complete()?;
    if parser.pos != sig.len() {
        return Err(SigFault::Bad("signature holds more than one complete type"));
    }
    Ok(ty)
}

/// Checks a signature carried as data (a `g` value) ([`parse_many`]).
pub(crate) fn check_data(sig: &[u8]) -> std::result::Result<(), SigFault> {
    let mut parser = Parser::new(sig, true)?;
    while parser.pos < sig.len() {
        parser.complete()?;
    }
    Ok(())
}

struct Parser<'a> {
    sig: &'a [u8],
    pos: usize,
    arrays: usize,
    structs: usize,
    /// Whether `h` parses (as data only; see [`check_data`]).
    fd_ok: bool,
}

impl<'a> Parser<'a> {
    const fn new(sig: &'a [u8], fd_ok: bool) -> std::result::Result<Self, SigFault> {
        if sig.len() > MAX_SIGNATURE {
            return Err(SigFault::Bad("signature longer than 255 bytes"));
        }
        Ok(Self {
            sig,
            pos: 0,
            arrays: 0,
            structs: 0,
            fd_ok,
        })
    }

    #[inline]
    fn peek(&self) -> Option<u8> {
        self.sig.get(self.pos).copied()
    }

    #[inline]
    const fn open_struct(&mut self) -> std::result::Result<(), SigFault> {
        if self.structs >= MAX_STRUCT_DEPTH {
            return Err(SigFault::Bad("structs nested deeper than 32"));
        }
        self.structs += 1;
        Ok(())
    }

    #[inline]
    const fn close_struct(&mut self) {
        self.structs = self.structs.saturating_sub(1);
    }

    fn complete(&mut self) -> std::result::Result<Type, SigFault> {
        let code = self
            .peek()
            .ok_or(SigFault::Bad("signature ends inside a type"))?;
        self.pos = self.pos.saturating_add(1);
        Ok(match code {
            b'y' => Type::Byte,
            b'b' => Type::Bool,
            b'n' => Type::I16,
            b'q' => Type::U16,
            b'i' => Type::I32,
            b'u' => Type::U32,
            b'x' => Type::I64,
            b't' => Type::U64,
            b'd' => Type::F64,
            b's' => Type::Str,
            b'o' => Type::Path,
            b'g' => Type::Sig,
            b'v' => Type::Variant,
            b'h' if self.fd_ok => Type::U32,
            b'h' => return Err(SigFault::Fd),
            b'a' => {
                if self.arrays >= MAX_ARRAY_DEPTH {
                    return Err(SigFault::Bad("arrays nested deeper than 32"));
                }
                self.arrays += 1;
                let ty = if self.peek() == Some(b'{') {
                    self.pos = self.pos.saturating_add(1);
                    self.open_struct()?;
                    let key = self.complete()?;
                    if !key.is_basic() {
                        return Err(SigFault::Bad("dict key is not a basic type"));
                    }
                    if self.peek() == Some(b'}') {
                        return Err(SigFault::Bad("dict entry holds other than two types"));
                    }
                    let value = self.complete()?;
                    if self.peek() != Some(b'}') {
                        return Err(SigFault::Bad("dict entry holds other than two types"));
                    }
                    self.pos = self.pos.saturating_add(1);
                    self.close_struct();
                    Type::dict(key, value)
                } else {
                    Type::array(self.complete()?)
                };
                self.arrays = self.arrays.saturating_sub(1);
                ty
            }
            b'(' => {
                self.open_struct()?;
                let mut fields = Vec::new();
                while self.peek() != Some(b')') {
                    fields.push(self.complete()?);
                }
                self.pos = self.pos.saturating_add(1);
                if fields.is_empty() {
                    return Err(SigFault::Bad("empty struct"));
                }
                self.close_struct();
                Type::Struct(fields.into())
            }
            b'{' => return Err(SigFault::Bad("dict entry outside an array")),
            b')' | b'}' => return Err(SigFault::Bad("unbalanced close")),
            _ => return Err(SigFault::Bad("unknown type code")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bad(sig: &str) -> &'static str {
        match parse_many(sig.as_bytes()) {
            Err(SigFault::Bad(what)) => what,
            other => panic!("{sig:?} parsed as {other:?}"),
        }
    }

    #[test]
    fn every_atspi_signature_round_trips() {
        for sig in [
            "",
            "s",
            "so",
            "(so)",
            "a(so)",
            "a{sv}",
            "a{ss}",
            "siiva{sv}",
            "(so)(so)(so)iiassusau",
            "a((so)(so)(so)iiassusau)",
            "a(ua(so))",
            "a(sss)",
            "sa{sv}as",
            "(iiii)",
            "a{yd}",
            "aay",
            "(y(t))",
        ] {
            let types = parse_signature(sig).unwrap();
            assert_eq!(signature_of(&types), sig);
        }
    }

    #[test]
    fn the_specification_limits_hold() {
        assert_eq!(bad("()"), "empty struct");
        assert_eq!(bad("a"), "signature ends inside a type");
        assert_eq!(bad("(s"), "signature ends inside a type");
        assert_eq!(bad("{sv}"), "dict entry outside an array");
        assert_eq!(bad("a{vs}"), "dict key is not a basic type");
        assert_eq!(bad("a{(s)s}"), "dict key is not a basic type");
        assert_eq!(bad("a{s}"), "dict entry holds other than two types");
        assert_eq!(bad("a{sss}"), "dict entry holds other than two types");
        assert_eq!(bad("s)"), "unbalanced close");
        assert_eq!(bad("m"), "unknown type code");
        assert_eq!(parse_many(b"ah"), Err(SigFault::Fd));
        for sig in ["h", "ah", "a{hs}", "(hh)", "sa{sv}h"] {
            assert_eq!(check_data(sig.as_bytes()), Ok(()), "{sig}");
        }
        assert_eq!(
            check_data(b"a{vh}"),
            Err(SigFault::Bad("dict key is not a basic type"))
        );
        assert_eq!(
            check_data(b"(h"),
            Err(SigFault::Bad("signature ends inside a type"))
        );
        assert_eq!(
            check_data(&[b'h'; 256]),
            Err(SigFault::Bad("signature longer than 255 bytes"))
        );
        let deep_arrays = "a".repeat(32) + "y";
        assert!(parse_many(deep_arrays.as_bytes()).is_ok());
        assert_eq!(bad(&("a".repeat(33) + "y")), "arrays nested deeper than 32");
        let deep = |n: usize| "(".repeat(n) + "y" + &")".repeat(n);
        assert!(parse_many(deep(32).as_bytes()).is_ok());
        assert_eq!(bad(&deep(33)), "structs nested deeper than 32");
        assert_eq!(bad(&"y".repeat(256)), "signature longer than 255 bytes");
        assert!(parse_many("y".repeat(255).as_bytes()).is_ok());
    }

    #[test]
    fn a_variant_signature_is_one_complete_type() {
        assert_eq!(Type::parse("(so)").unwrap().signature(), "(so)");
        assert!(matches!(Type::parse(""), Err(Error::Invalid(_))));
        assert!(matches!(Type::parse("ss"), Err(Error::Invalid(_))));
        assert!(matches!(Type::parse("h"), Err(Error::Unsupported(_))));
    }
}
