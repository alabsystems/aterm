// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Deserialization: `serde::Deserializer` over the parse tree.
//!
//! # Why this half exists at all
//!
//! aterm calls `from_str` 168 times, and 43 of those are typed
//! (`from_str::<Config>`), standing on 161 `#[derive(Deserialize)]` models. A
//! replacement that handed back an untyped value tree would mean rewriting all
//! 161 by hand — so the replacement is a `Deserializer`, and serde stays.
//!
//! # Spans
//!
//! The config editor underlines schema errors, not just syntax errors, so a
//! failure raised deep inside a `Deserialize` impl has to come back carrying a
//! byte range. Each node attaches its own span on the way out, and
//! [`crate::Error::with_span`] keeps the FIRST one set — which is the innermost,
//! most specific node that could have been at fault.

use core::fmt;
use core::ops::Range;

use serde::de::{
    DeserializeOwned, DeserializeSeed, EnumAccess, MapAccess, SeqAccess, VariantAccess, Visitor,
};
use serde::forward_to_deserialize_any;

use crate::Result;
use crate::datetime::{DATETIME_FIELD, DATETIME_STRUCT};
use crate::edit::{ArrayOfTables, Item, Key, Table, Value as EValue};

#[doc(inline)]
pub use crate::error::Error;

/// Parse `text` and build `T` out of it.
///
/// # Errors
/// If `text` is not valid TOML, or does not have `T`'s shape.
pub fn from_str<T: DeserializeOwned>(text: &str) -> Result<T> {
    let document = crate::edit::parse_document(text, crate::edit::ParseLimits::default())?;
    T::deserialize(Deserializer::new(
        Some(text),
        Node::from_item(document.as_item()),
    ))
}

/// A position in the parse tree. Values and tables are separate arms because a
/// table reached through `[header]` and one reached through `{ inline }` are
/// different nodes carrying the same meaning.
#[derive(Clone, Copy)]
enum Node<'a> {
    Missing,
    Value(&'a EValue),
    Table(&'a Table),
    ArrayOfTables(&'a ArrayOfTables),
}

impl<'a> Node<'a> {
    fn from_item(item: &'a Item) -> Self {
        match item {
            Item::None => Node::Missing,
            Item::Value(v) => Node::Value(v),
            Item::Table(t) => Node::Table(t),
            Item::ArrayOfTables(a) => Node::ArrayOfTables(a),
        }
    }

    fn span(self) -> Option<Range<usize>> {
        match self {
            Node::Missing | Node::ArrayOfTables(_) => None,
            Node::Value(v) => v.span(),
            Node::Table(t) => t.span(),
        }
    }

    fn type_name(self) -> &'static str {
        match self {
            Node::Missing => "nothing",
            Node::Value(v) => v.type_name(),
            Node::Table(_) => "table",
            Node::ArrayOfTables(_) => "array of tables",
        }
    }
}

struct Deserializer<'a> {
    source: Option<&'a str>,
    node: Node<'a>,
}

impl<'a> Deserializer<'a> {
    fn new(source: Option<&'a str>, node: Node<'a>) -> Self {
        Self { source, node }
    }

    fn item(source: Option<&'a str>, item: &'a Item) -> Self {
        Self {
            source,
            node: Node::from_item(item),
        }
    }

    fn locate(&self, error: Error) -> Error {
        error.with_span(self.source, self.node.span())
    }

    fn mismatch<T>(&self, expected: &str) -> Result<T> {
        Err(self.locate(Error::message(format!(
            "invalid type: {}, expected {expected}",
            self.node.type_name()
        ))))
    }
}

impl<'de, 'a> serde::Deserializer<'de> for Deserializer<'a> {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        let result = match self.node {
            Node::Missing => return self.mismatch("a value"),
            Node::Table(t) => visitor.visit_map(TableAccess::new(self.source, t.items.iter())),
            Node::ArrayOfTables(a) => visitor.visit_seq(TableSeqAccess {
                source: self.source,
                tables: a.values.iter(),
            }),
            Node::Value(value) => match value {
                EValue::String(v) => visitor.visit_str(v.value()),
                EValue::Integer(v) => visitor.visit_i64(*v.value()),
                EValue::Float(v) => visitor.visit_f64(*v.value()),
                EValue::Boolean(v) => visitor.visit_bool(*v.value()),
                EValue::Datetime(v) => visitor.visit_map(DatetimeAccess::new(v.value())),
                EValue::Array(a) => visitor.visit_seq(ValueSeqAccess {
                    source: self.source,
                    values: a.iter(),
                }),
                EValue::InlineTable(t) => {
                    visitor.visit_map(TableAccess::new(self.source, t.items.iter()))
                }
            },
        };
        result.map_err(|e| self.locate(e))
    }

    /// A key that is absent never reaches the deserializer at all — the map
    /// access simply never yields it — so anything that DOES arrive here is a
    /// present value.
    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        visitor.visit_some(self)
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value> {
        // A date-time has no serde data-model type, so it crosses as a struct
        // under one reserved name. Only that exact name is special-cased, so a
        // user struct cannot accidentally claim the protocol.
        if name == DATETIME_STRUCT
            && let Node::Value(EValue::Datetime(v)) = self.node
        {
            return visitor
                .visit_map(DatetimeAccess::new(v.value()))
                .map_err(|e| self.locate(e));
        }
        self.deserialize_any(visitor)
    }

    /// TOML spells externally-tagged enums two ways: a bare string for a unit
    /// variant, and a one-key table for everything else.
    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value> {
        let result = match self.node {
            Node::Value(EValue::String(v)) => visitor.visit_enum(UnitVariant {
                name: v.value().as_str(),
            }),
            Node::Value(EValue::InlineTable(t)) if t.items.len() == 1 => {
                let (key, item) = t.items.iter().next().expect("length checked");
                visitor.visit_enum(TableVariant {
                    source: self.source,
                    key,
                    item,
                })
            }
            Node::Table(t) if t.items.len() == 1 => {
                let (key, item) = t.items.iter().next().expect("length checked");
                visitor.visit_enum(TableVariant {
                    source: self.source,
                    key,
                    item,
                })
            }
            _ => return self.mismatch("a string, or a table with exactly one key"),
        };
        result.map_err(|e| self.locate(e))
    }

    forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf unit unit_struct seq tuple tuple_struct map identifier
        ignored_any
    }
}

// ---- map and sequence access ----------------------------------------------

struct TableAccess<'a, I> {
    source: Option<&'a str>,
    entries: I,
    pending: Option<&'a Item>,
}

impl<'a, I> TableAccess<'a, I> {
    fn new(source: Option<&'a str>, entries: I) -> Self {
        Self {
            source,
            entries,
            pending: None,
        }
    }
}

impl<'de, 'a, I: Iterator<Item = (&'a Key, &'a Item)>> MapAccess<'de> for TableAccess<'a, I> {
    type Error = Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(&mut self, seed: K) -> Result<Option<K::Value>> {
        let Some((key, item)) = self.entries.next() else {
            return Ok(None);
        };
        self.pending = Some(item);
        seed.deserialize(KeyDeserializer {
            source: self.source,
            key,
        })
        .map(Some)
        .map_err(|e| e.with_span(self.source, key.span()))
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value> {
        let item = self
            .pending
            .take()
            .expect("serde asks for a value only after a key");
        seed.deserialize(Deserializer::item(self.source, item))
            .map_err(|e| e.with_span(self.source, item.span()))
    }
}

struct ValueSeqAccess<'a, I> {
    source: Option<&'a str>,
    values: I,
}

impl<'de, 'a, I: Iterator<Item = &'a EValue>> SeqAccess<'de> for ValueSeqAccess<'a, I> {
    type Error = Error;

    fn next_element_seed<T: DeserializeSeed<'de>>(&mut self, seed: T) -> Result<Option<T::Value>> {
        let Some(value) = self.values.next() else {
            return Ok(None);
        };
        seed.deserialize(Deserializer::new(self.source, Node::Value(value)))
            .map(Some)
            .map_err(|e| e.with_span(self.source, value.span()))
    }
}

struct TableSeqAccess<'a, I> {
    source: Option<&'a str>,
    tables: I,
}

impl<'de, 'a, I: Iterator<Item = &'a Table>> SeqAccess<'de> for TableSeqAccess<'a, I> {
    type Error = Error;

    fn next_element_seed<T: DeserializeSeed<'de>>(&mut self, seed: T) -> Result<Option<T::Value>> {
        let Some(table) = self.tables.next() else {
            return Ok(None);
        };
        seed.deserialize(Deserializer::new(self.source, Node::Table(table)))
            .map(Some)
            .map_err(|e| e.with_span(self.source, table.span()))
    }
}

/// Feeds a table key to whatever the target type wants it as — a field
/// identifier, a `String`, or a unit enum variant.
struct KeyDeserializer<'a> {
    source: Option<&'a str>,
    key: &'a Key,
}

impl<'de> serde::Deserializer<'de> for KeyDeserializer<'_> {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        visitor.visit_str(self.key.get())
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value> {
        visitor
            .visit_enum(UnitVariant {
                name: self.key.get(),
            })
            .map_err(|e| e.with_span(self.source, self.key.span()))
    }

    forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf option unit unit_struct newtype_struct seq tuple
        tuple_struct map struct identifier ignored_any
    }
}

/// The one-entry map a date-time crosses serde as.
struct DatetimeAccess<'a> {
    value: &'a crate::Datetime,
    done: bool,
}

impl<'a> DatetimeAccess<'a> {
    fn new(value: &'a crate::Datetime) -> Self {
        Self { value, done: false }
    }
}

impl<'de> MapAccess<'de> for DatetimeAccess<'_> {
    type Error = Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(&mut self, seed: K) -> Result<Option<K::Value>> {
        if self.done {
            return Ok(None);
        }
        self.done = true;
        seed.deserialize(StrDeserializer {
            text: DATETIME_FIELD,
        })
        .map(Some)
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value> {
        seed.deserialize(StringDeserializer {
            text: self.value.to_string(),
        })
    }
}

struct StrDeserializer<'a> {
    text: &'a str,
}

impl<'de> serde::Deserializer<'de> for StrDeserializer<'_> {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        visitor.visit_str(self.text)
    }

    forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf option unit unit_struct newtype_struct seq tuple
        tuple_struct map struct enum identifier ignored_any
    }
}

struct StringDeserializer {
    text: String,
}

impl<'de> serde::Deserializer<'de> for StringDeserializer {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        visitor.visit_string(self.text)
    }

    forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf option unit unit_struct newtype_struct seq tuple
        tuple_struct map struct enum identifier ignored_any
    }
}

// ---- enums ----------------------------------------------------------------

struct UnitVariant<'a> {
    name: &'a str,
}

impl<'de> EnumAccess<'de> for UnitVariant<'_> {
    type Error = Error;
    type Variant = Self;

    fn variant_seed<V: DeserializeSeed<'de>>(self, seed: V) -> Result<(V::Value, Self)> {
        let name = seed.deserialize(StrDeserializer { text: self.name })?;
        Ok((name, self))
    }
}

impl<'de> VariantAccess<'de> for UnitVariant<'_> {
    type Error = Error;

    fn unit_variant(self) -> Result<()> {
        Ok(())
    }

    fn newtype_variant_seed<T: DeserializeSeed<'de>>(self, _seed: T) -> Result<T::Value> {
        Err(Error::message(format!(
            "expected a table for the `{}` variant, found a bare string",
            self.name
        )))
    }

    fn tuple_variant<V: Visitor<'de>>(self, _len: usize, _visitor: V) -> Result<V::Value> {
        Err(Error::message(format!(
            "expected a table for the `{}` variant, found a bare string",
            self.name
        )))
    }

    fn struct_variant<V: Visitor<'de>>(
        self,
        _fields: &'static [&'static str],
        _visitor: V,
    ) -> Result<V::Value> {
        Err(Error::message(format!(
            "expected a table for the `{}` variant, found a bare string",
            self.name
        )))
    }
}

struct TableVariant<'a> {
    source: Option<&'a str>,
    key: &'a Key,
    item: &'a Item,
}

impl<'de, 'a> EnumAccess<'de> for TableVariant<'a> {
    type Error = Error;
    type Variant = VariantValue<'a>;

    fn variant_seed<V: DeserializeSeed<'de>>(self, seed: V) -> Result<(V::Value, Self::Variant)> {
        let name = seed.deserialize(StrDeserializer {
            text: self.key.get(),
        })?;
        Ok((
            name,
            VariantValue {
                source: self.source,
                item: self.item,
            },
        ))
    }
}

struct VariantValue<'a> {
    source: Option<&'a str>,
    item: &'a Item,
}

impl<'de> VariantAccess<'de> for VariantValue<'_> {
    type Error = Error;

    fn unit_variant(self) -> Result<()> {
        Ok(())
    }

    fn newtype_variant_seed<T: DeserializeSeed<'de>>(self, seed: T) -> Result<T::Value> {
        seed.deserialize(Deserializer::item(self.source, self.item))
    }

    fn tuple_variant<V: Visitor<'de>>(self, _len: usize, visitor: V) -> Result<V::Value> {
        serde::Deserializer::deserialize_any(Deserializer::item(self.source, self.item), visitor)
    }

    fn struct_variant<V: Visitor<'de>>(
        self,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value> {
        serde::Deserializer::deserialize_any(Deserializer::item(self.source, self.item), visitor)
    }
}

// ---- the untyped value tree -----------------------------------------------

impl<'de> serde::Deserialize<'de> for crate::Value {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> core::result::Result<Self, D::Error> {
        deserializer.deserialize_any(ValueVisitor)
    }
}

struct ValueVisitor;

impl<'de> Visitor<'de> for ValueVisitor {
    type Value = crate::Value;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("any valid TOML value")
    }

    fn visit_bool<E>(self, v: bool) -> core::result::Result<Self::Value, E> {
        Ok(crate::Value::Boolean(v))
    }

    fn visit_i64<E>(self, v: i64) -> core::result::Result<Self::Value, E> {
        Ok(crate::Value::Integer(v))
    }

    fn visit_u64<E: serde::de::Error>(self, v: u64) -> core::result::Result<Self::Value, E> {
        i64::try_from(v)
            .map(crate::Value::Integer)
            .map_err(|_| E::custom("integer does not fit in TOML's signed 64-bit range"))
    }

    fn visit_f64<E>(self, v: f64) -> core::result::Result<Self::Value, E> {
        Ok(crate::Value::Float(v))
    }

    fn visit_str<E>(self, v: &str) -> core::result::Result<Self::Value, E> {
        Ok(crate::Value::String(v.to_owned()))
    }

    fn visit_string<E>(self, v: String) -> core::result::Result<Self::Value, E> {
        Ok(crate::Value::String(v))
    }

    fn visit_some<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> core::result::Result<Self::Value, D::Error> {
        deserializer.deserialize_any(self)
    }

    fn visit_seq<A: SeqAccess<'de>>(
        self,
        mut seq: A,
    ) -> core::result::Result<Self::Value, A::Error> {
        let mut out = Vec::new();
        while let Some(value) = seq.next_element()? {
            out.push(value);
        }
        Ok(crate::Value::Array(out))
    }

    fn visit_map<A: MapAccess<'de>>(
        self,
        mut map: A,
    ) -> core::result::Result<Self::Value, A::Error> {
        let mut table = crate::Table::new();
        let Some(first) = map.next_key::<String>()? else {
            return Ok(crate::Value::Table(table));
        };
        // The date-time protocol: a one-entry map under the reserved field
        // name is a date-time, not a table with a very odd key.
        if first == DATETIME_FIELD {
            let text: String = map.next_value()?;
            let parsed = text.parse().map_err(serde::de::Error::custom)?;
            return Ok(crate::Value::Datetime(parsed));
        }
        table.insert(first, map.next_value()?);
        while let Some(key) = map.next_key::<String>()? {
            table.insert(key, map.next_value()?);
        }
        Ok(crate::Value::Table(table))
    }
}

impl<'de> serde::Deserialize<'de> for crate::Table {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> core::result::Result<Self, D::Error> {
        match crate::Value::deserialize(deserializer)? {
            crate::Value::Table(t) => Ok(t),
            other => Err(serde::de::Error::custom(format!(
                "invalid type: {}, expected a table",
                other.type_str()
            ))),
        }
    }
}

/// Deserialize straight out of an already-parsed [`crate::Value`].
///
/// Routed through the edit tree rather than through a second `Deserializer`
/// implementation: one deserializer means one answer to every coercion
/// question, which is the same reason there is only one parser.
impl<'de> serde::Deserializer<'de> for crate::Value {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        let item = crate::ser::value_to_item(&self)?;
        serde::Deserializer::deserialize_any(Deserializer::item(None, &item), visitor)
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        visitor.visit_some(self)
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value> {
        let item = crate::ser::value_to_item(&self)?;
        serde::Deserializer::deserialize_struct(
            Deserializer::item(None, &item),
            name,
            fields,
            visitor,
        )
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value> {
        let item = crate::ser::value_to_item(&self)?;
        serde::Deserializer::deserialize_enum(
            Deserializer::item(None, &item),
            name,
            variants,
            visitor,
        )
    }

    forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf unit unit_struct seq tuple tuple_struct map identifier
        ignored_any
    }
}
