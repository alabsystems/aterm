// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`Value`]: one D-Bus value of any type the crate speaks, owning its data.
//!
//! An array or dict carries its element type, so an empty one still has a
//! signature; the constructors refuse an element of another type, which keeps
//! every [`Value`] encodable as its [`Value::type_of`].
//!
//! A `Value` is 32 bytes and every decoded value consumed at least one input
//! byte, while types are shared, not copied ([`Type`]): decoding `n` bytes
//! allocates O(`n`), about 32`n` for the worst case (`ay`).

use crate::error::{Error, Result};
use crate::names;
use crate::sig::{self, Type};

/// A validated object path.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectPath(String);

impl ObjectPath {
    /// # Errors
    ///
    /// [`Error::Invalid`] unless `path` is an object path.
    pub fn new(path: impl Into<String>) -> Result<Self> {
        let path = path.into();
        if names::is_object_path(&path) {
            Ok(Self(path))
        } else {
            Err(Error::Invalid("object path"))
        }
    }

    #[inline]
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The decoder's constructor, for a path it has just validated.
    #[inline]
    pub(crate) const fn checked(path: String) -> Self {
        Self(path)
    }
}

/// A validated signature: zero or more complete types. As data it may name
/// `h`, which [`sig::parse_signature`] refuses for a body or a variant.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Signature(String);

impl Signature {
    /// # Errors
    ///
    /// [`Error::Invalid`] for a signature the specification forbids.
    pub fn new(sig: impl Into<String>) -> Result<Self> {
        let sig = sig.into();
        sig::check_data(sig.as_bytes()).map_err(sig::SigFault::refused)?;
        Ok(Self(sig))
    }

    #[inline]
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The decoder's constructor, for a signature it has just parsed.
    #[inline]
    pub(crate) const fn checked(sig: String) -> Self {
        Self(sig)
    }
}

/// An array: its element type and the elements, every one of that type.
#[derive(Clone, Debug, PartialEq)]
pub struct Array {
    elem: Type,
    items: Vec<Value>,
}

impl Array {
    #[inline]
    pub(crate) const fn checked(elem: Type, items: Vec<Value>) -> Self {
        Self { elem, items }
    }

    #[inline]
    #[must_use]
    pub const fn elem(&self) -> &Type {
        &self.elem
    }

    #[inline]
    #[must_use]
    pub fn items(&self) -> &[Value] {
        &self.items
    }
}

/// A dict (`a{kv}`): its key and value types and the entries, in wire order.
#[derive(Clone, Debug, PartialEq)]
pub struct Dict {
    key: Type,
    value: Type,
    entries: Vec<(Value, Value)>,
}

impl Dict {
    #[inline]
    pub(crate) const fn checked(key: Type, value: Type, entries: Vec<(Value, Value)>) -> Self {
        Self {
            key,
            value,
            entries,
        }
    }

    #[inline]
    #[must_use]
    pub const fn key(&self) -> &Type {
        &self.key
    }

    #[inline]
    #[must_use]
    pub const fn value(&self) -> &Type {
        &self.value
    }

    #[inline]
    #[must_use]
    pub fn entries(&self) -> &[(Value, Value)] {
        &self.entries
    }

    /// The value under the first entry whose key is the string `key`.
    #[inline]
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.entries
            .iter()
            .find(|(k, _)| k.as_str() == Some(key))
            .map(|(_, v)| v)
    }
}

/// One D-Bus value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Byte(u8),
    Bool(bool),
    I16(i16),
    U16(u16),
    I32(i32),
    U32(u32),
    I64(i64),
    U64(u64),
    F64(f64),
    /// No NUL inside; the encoder refuses one.
    Str(String),
    Path(ObjectPath),
    Sig(Signature),
    Variant(Box<Value>),
    Array(Box<Array>),
    Dict(Box<Dict>),
    /// One or more fields; the encoder refuses an empty struct.
    Struct(Vec<Value>),
}

impl Value {
    /// A variant holding `inner`.
    #[inline]
    #[must_use]
    pub fn variant(inner: Value) -> Self {
        Self::Variant(Box::new(inner))
    }

    /// An array of `elem` values.
    ///
    /// # Errors
    ///
    /// [`Error::Invalid`] when an item is not of type `elem`.
    pub fn array(elem: Type, items: Vec<Value>) -> Result<Self> {
        if items.iter().any(|item| !item.is(&elem)) {
            return Err(Error::Invalid("array element of another type"));
        }
        Ok(Self::Array(Box::new(Array { elem, items })))
    }

    /// A dict from `key` to `value` values, entries kept in the order given.
    ///
    /// # Errors
    ///
    /// [`Error::Invalid`] when `key` is not a basic type or an entry is not of
    /// the declared types.
    pub fn dict(key: Type, value: Type, entries: Vec<(Value, Value)>) -> Result<Self> {
        if !key.is_basic() {
            return Err(Error::Invalid("dict key is not a basic type"));
        }
        if entries.iter().any(|(k, v)| !k.is(&key) || !v.is(&value)) {
            return Err(Error::Invalid("dict entry of another type"));
        }
        Ok(Self::Dict(Box::new(Dict {
            key,
            value,
            entries,
        })))
    }

    /// An `as`.
    #[must_use]
    pub fn strings<S: Into<String>>(items: impl IntoIterator<Item = S>) -> Self {
        Self::Array(Box::new(Array {
            elem: Type::Str,
            items: items.into_iter().map(|s| Self::Str(s.into())).collect(),
        }))
    }

    /// An object path value.
    ///
    /// # Errors
    ///
    /// [`Error::Invalid`] unless `path` is an object path.
    #[inline]
    pub fn path(path: &str) -> Result<Self> {
        ObjectPath::new(path).map(Self::Path)
    }

    /// This value's type.
    #[must_use]
    pub fn type_of(&self) -> Type {
        match self {
            Self::Byte(_) => Type::Byte,
            Self::Bool(_) => Type::Bool,
            Self::I16(_) => Type::I16,
            Self::U16(_) => Type::U16,
            Self::I32(_) => Type::I32,
            Self::U32(_) => Type::U32,
            Self::I64(_) => Type::I64,
            Self::U64(_) => Type::U64,
            Self::F64(_) => Type::F64,
            Self::Str(_) => Type::Str,
            Self::Path(_) => Type::Path,
            Self::Sig(_) => Type::Sig,
            Self::Variant(_) => Type::Variant,
            Self::Array(a) => Type::Array(a.elem.clone().into()),
            Self::Dict(d) => Type::Dict(d.key.clone().into(), d.value.clone().into()),
            Self::Struct(fields) => Type::Struct(fields.iter().map(Self::type_of).collect()),
        }
    }

    /// Whether this value is of type `ty`, without building its type.
    #[must_use]
    pub fn is(&self, ty: &Type) -> bool {
        match (self, ty) {
            (Self::Byte(_), Type::Byte)
            | (Self::Bool(_), Type::Bool)
            | (Self::I16(_), Type::I16)
            | (Self::U16(_), Type::U16)
            | (Self::I32(_), Type::I32)
            | (Self::U32(_), Type::U32)
            | (Self::I64(_), Type::I64)
            | (Self::U64(_), Type::U64)
            | (Self::F64(_), Type::F64)
            | (Self::Str(_), Type::Str)
            | (Self::Path(_), Type::Path)
            | (Self::Sig(_), Type::Sig)
            | (Self::Variant(_), Type::Variant) => true,
            (Self::Array(a), Type::Array(elem)) => a.elem == **elem,
            (Self::Dict(d), Type::Dict(key, value)) => d.key == **key && d.value == **value,
            (Self::Struct(fields), Type::Struct(types)) => {
                fields.len() == types.len() && fields.iter().zip(types.iter()).all(|(f, t)| f.is(t))
            }
            _ => false,
        }
    }

    #[inline]
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            Self::Path(p) => Some(p.as_str()),
            Self::Sig(s) => Some(s.as_str()),
            _ => None,
        }
    }

    #[inline]
    #[must_use]
    pub const fn as_bool(&self) -> Option<bool> {
        if let Self::Bool(b) = self {
            Some(*b)
        } else {
            None
        }
    }

    #[inline]
    #[must_use]
    pub const fn as_i32(&self) -> Option<i32> {
        if let Self::I32(v) = self {
            Some(*v)
        } else {
            None
        }
    }

    #[inline]
    #[must_use]
    pub const fn as_u32(&self) -> Option<u32> {
        if let Self::U32(v) = self {
            Some(*v)
        } else {
            None
        }
    }

    #[inline]
    #[must_use]
    pub const fn as_f64(&self) -> Option<f64> {
        if let Self::F64(v) = self {
            Some(*v)
        } else {
            None
        }
    }

    /// The value inside a variant.
    #[inline]
    #[must_use]
    pub fn as_variant(&self) -> Option<&Value> {
        if let Self::Variant(v) = self {
            Some(v)
        } else {
            None
        }
    }

    #[inline]
    #[must_use]
    pub fn as_struct(&self) -> Option<&[Value]> {
        if let Self::Struct(fields) = self {
            Some(fields)
        } else {
            None
        }
    }

    #[inline]
    #[must_use]
    pub fn as_array(&self) -> Option<&Array> {
        if let Self::Array(a) = self {
            Some(a)
        } else {
            None
        }
    }

    #[inline]
    #[must_use]
    pub fn as_dict(&self) -> Option<&Dict> {
        if let Self::Dict(d) = self {
            Some(d)
        } else {
            None
        }
    }
}

macro_rules! from_scalar {
    ($($t:ty => $v:ident),* $(,)?) => {$(
        impl From<$t> for Value {
            #[inline]
            fn from(v: $t) -> Self {
                Self::$v(v)
            }
        }
    )*};
}

from_scalar!(u8 => Byte, bool => Bool, i16 => I16, u16 => U16, i32 => I32, u32 => U32,
    i64 => I64, u64 => U64, f64 => F64, String => Str, ObjectPath => Path, Signature => Sig);

impl From<&str> for Value {
    #[inline]
    fn from(s: &str) -> Self {
        Self::Str(s.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn containers_refuse_a_foreign_element() {
        assert!(Value::array(Type::U32, vec![Value::U32(1), Value::I32(2)]).is_err());
        assert!(Value::dict(Type::Variant, Type::Str, vec![]).is_err());
        assert!(Value::dict(Type::Str, Type::Variant, vec![("k".into(), Value::U32(1))]).is_err());
        let sv = Value::dict(
            Type::Str,
            Type::Variant,
            vec![("k".into(), Value::variant(1u32.into()))],
        )
        .unwrap();
        assert_eq!(sv.type_of().signature(), "a{sv}");
        assert_eq!(
            sv.as_dict().unwrap().get("k").and_then(Value::as_variant),
            Some(&Value::U32(1))
        );
        let empty = Value::array(Type::structure([Type::Str, Type::Path]), vec![]).unwrap();
        assert_eq!(empty.type_of().signature(), "a(so)");
        assert!(ObjectPath::new("/a/").is_err() && Signature::new("a").is_err());
    }

    #[test]
    fn a_value_stays_small() {
        assert!(
            std::mem::size_of::<Value>() <= 32,
            "{}",
            std::mem::size_of::<Value>()
        );
    }
}
