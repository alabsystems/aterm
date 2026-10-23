// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Serialization via an [`crate::edit::Item`] tree encoded by the document printer.
//!
//! A `None` struct field is omitted, while `None` in an array, map value, variant payload,
//! `Some`, or newtype is refused because TOML has no null.

use serde::ser::{Impossible, Serialize};

use crate::Result;
use crate::datetime::{DATETIME_FIELD, DATETIME_STRUCT};
use crate::edit::{Array, ArrayOfTables, DocumentMut, InlineTable, Item, Table, Value as EValue};

#[doc(inline)]
pub use crate::error::Error;

const UNSUPPORTED_NONE: &str = "TOML has no null: a `None` field is left out, but a `None` array \
                                element, map value, variant payload, `Some` or newtype cannot be \
                                written";

fn written(item: Item) -> Result<Item> {
    if item.is_none() {
        return Err(Error::message(UNSUPPORTED_NONE));
    }
    Ok(item)
}

/// Render `value` as a TOML document.
///
/// # Errors
/// If the value is not a table at the top level, or contains something TOML
/// cannot express.
pub fn to_string<T: Serialize + ?Sized>(value: &T) -> Result<String> {
    let item = value.serialize(ItemSerializer)?;
    match item {
        Item::Table(table) => {
            let document = DocumentMut {
                root: Item::Table(table),
                ..DocumentMut::default()
            };
            Ok(document.to_string())
        }
        Item::Value(EValue::InlineTable(inline)) => {
            let mut table = Table::new();
            for (key, item) in inline.items.iter() {
                table.insert_formatted(key.clone(), item.clone());
            }
            let document = DocumentMut {
                root: Item::Table(table),
                ..DocumentMut::default()
            };
            Ok(document.to_string())
        }
        other => Err(Error::message(format!(
            "a TOML document must be a table at the top level, not {}",
            other.type_name()
        ))),
    }
}

/// Convert an untyped [`crate::Value`] into an [`Item`].
pub(crate) fn value_to_item(value: &crate::Value) -> Result<Item> {
    Ok(match value {
        crate::Value::String(s) => Item::Value(EValue::from(s.clone())),
        crate::Value::Integer(v) => Item::Value(EValue::from(*v)),
        crate::Value::Float(v) => Item::Value(EValue::from(*v)),
        crate::Value::Boolean(v) => Item::Value(EValue::from(*v)),
        crate::Value::Datetime(v) => Item::Value(EValue::from(*v)),
        crate::Value::Array(values) => {
            let mut items = Vec::with_capacity(values.len());
            for value in values {
                items.push(value_to_item(value)?);
            }
            seq_to_item(items)?
        }
        crate::Value::Table(map) => {
            let mut table = Table::new();
            for (key, value) in map {
                table.insert(key, value_to_item(value)?);
            }
            Item::Table(table)
        }
    })
}

fn seq_to_item(items: Vec<Item>) -> Result<Item> {
    if !items.is_empty() && items.iter().all(Item::is_table) {
        let mut array = ArrayOfTables::new();
        for item in items {
            match item {
                Item::Table(t) => array.push(t),
                _ => unreachable!("just checked every element is a table"),
            }
        }
        return Ok(Item::ArrayOfTables(array));
    }
    let mut array = Array::new();
    for item in items {
        array.push(item_to_value(item)?);
    }
    Ok(Item::Value(EValue::Array(array)))
}

fn item_to_value(item: Item) -> Result<EValue> {
    Ok(match item {
        Item::Value(v) => v,
        Item::Table(t) => EValue::InlineTable(table_to_inline(t)?),
        Item::ArrayOfTables(tables) => {
            let mut array = Array::new();
            for table in tables.values {
                array.push(EValue::InlineTable(table_to_inline(table)?));
            }
            EValue::Array(array)
        }
        Item::None => return Err(Error::message(UNSUPPORTED_NONE)),
    })
}

fn table_to_inline(table: Table) -> Result<InlineTable> {
    let mut inline = InlineTable::new();
    for (key, item) in table.items.iter() {
        inline
            .items
            .insert(key.clone(), Item::Value(item_to_value(item.clone())?));
    }
    Ok(inline)
}

fn unsupported<T>(what: &str) -> Result<T> {
    Err(Error::message(format!("TOML cannot represent {what}")))
}

struct ItemSerializer;

impl serde::Serializer for ItemSerializer {
    type Ok = Item;
    type Error = Error;
    type SerializeSeq = SeqSerializer;
    type SerializeTuple = SeqSerializer;
    type SerializeTupleStruct = SeqSerializer;
    type SerializeTupleVariant = VariantSeqSerializer;
    type SerializeMap = TableSerializer;
    type SerializeStruct = StructSerializer;
    type SerializeStructVariant = VariantTableSerializer;

    fn serialize_bool(self, v: bool) -> Result<Item> {
        Ok(Item::Value(EValue::from(v)))
    }

    fn serialize_i8(self, v: i8) -> Result<Item> {
        self.serialize_i64(i64::from(v))
    }

    fn serialize_i16(self, v: i16) -> Result<Item> {
        self.serialize_i64(i64::from(v))
    }

    fn serialize_i32(self, v: i32) -> Result<Item> {
        self.serialize_i64(i64::from(v))
    }

    fn serialize_i64(self, v: i64) -> Result<Item> {
        Ok(Item::Value(EValue::from(v)))
    }

    fn serialize_i128(self, v: i128) -> Result<Item> {
        i64::try_from(v)
            .map(|v| Item::Value(EValue::from(v)))
            .map_err(|_| Error::message("integer does not fit in TOML's signed 64-bit range"))
    }

    fn serialize_u8(self, v: u8) -> Result<Item> {
        self.serialize_i64(i64::from(v))
    }

    fn serialize_u16(self, v: u16) -> Result<Item> {
        self.serialize_i64(i64::from(v))
    }

    fn serialize_u32(self, v: u32) -> Result<Item> {
        self.serialize_i64(i64::from(v))
    }

    fn serialize_u64(self, v: u64) -> Result<Item> {
        i64::try_from(v)
            .map(|v| Item::Value(EValue::from(v)))
            .map_err(|_| Error::message("integer does not fit in TOML's signed 64-bit range"))
    }

    fn serialize_u128(self, v: u128) -> Result<Item> {
        i64::try_from(v)
            .map(|v| Item::Value(EValue::from(v)))
            .map_err(|_| Error::message("integer does not fit in TOML's signed 64-bit range"))
    }

    fn serialize_f32(self, v: f32) -> Result<Item> {
        self.serialize_f64(f64::from(v))
    }

    fn serialize_f64(self, v: f64) -> Result<Item> {
        Ok(Item::Value(EValue::from(v)))
    }

    fn serialize_char(self, v: char) -> Result<Item> {
        self.serialize_str(v.encode_utf8(&mut [0u8; 4]))
    }

    fn serialize_str(self, v: &str) -> Result<Item> {
        Ok(Item::Value(EValue::from(v)))
    }

    fn serialize_bytes(self, v: &[u8]) -> Result<Item> {
        let mut array = Array::new();
        for byte in v {
            array.push(EValue::from(i64::from(*byte)));
        }
        Ok(Item::Value(EValue::Array(array)))
    }

    fn serialize_none(self) -> Result<Item> {
        Ok(Item::None)
    }

    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<Item> {
        written(value.serialize(self)?)
    }

    fn serialize_unit(self) -> Result<Item> {
        unsupported("a unit value")
    }

    fn serialize_unit_struct(self, _name: &'static str) -> Result<Item> {
        unsupported("a unit struct")
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
    ) -> Result<Item> {
        Ok(Item::Value(EValue::from(variant)))
    }

    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<Item> {
        written(value.serialize(self)?)
    }

    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<Item> {
        let mut table = Table::new();
        table.insert(variant, written(value.serialize(ItemSerializer)?)?);
        Ok(Item::Table(table))
    }

    fn serialize_seq(self, len: Option<usize>) -> Result<SeqSerializer> {
        Ok(SeqSerializer {
            items: Vec::with_capacity(len.unwrap_or(0)),
        })
    }

    fn serialize_tuple(self, len: usize) -> Result<SeqSerializer> {
        self.serialize_seq(Some(len))
    }

    fn serialize_tuple_struct(self, _name: &'static str, len: usize) -> Result<SeqSerializer> {
        self.serialize_seq(Some(len))
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<VariantSeqSerializer> {
        Ok(VariantSeqSerializer {
            variant,
            inner: SeqSerializer {
                items: Vec::with_capacity(len),
            },
        })
    }

    fn serialize_map(self, _len: Option<usize>) -> Result<TableSerializer> {
        Ok(TableSerializer {
            table: Table::new(),
            key: None,
        })
    }

    fn serialize_struct(self, name: &'static str, _len: usize) -> Result<StructSerializer> {
        Ok(StructSerializer {
            table: Table::new(),
            datetime: name == DATETIME_STRUCT,
        })
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
        _len: usize,
    ) -> Result<VariantTableSerializer> {
        Ok(VariantTableSerializer {
            variant,
            table: Table::new(),
        })
    }
}

struct SeqSerializer {
    items: Vec<Item>,
}

impl SeqSerializer {
    fn push<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<()> {
        self.items.push(value.serialize(ItemSerializer)?);
        Ok(())
    }
}

impl serde::ser::SerializeSeq for SeqSerializer {
    type Ok = Item;
    type Error = Error;

    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<()> {
        self.push(value)
    }

    fn end(self) -> Result<Item> {
        seq_to_item(self.items)
    }
}

impl serde::ser::SerializeTuple for SeqSerializer {
    type Ok = Item;
    type Error = Error;

    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<()> {
        self.push(value)
    }

    fn end(self) -> Result<Item> {
        seq_to_item(self.items)
    }
}

impl serde::ser::SerializeTupleStruct for SeqSerializer {
    type Ok = Item;
    type Error = Error;

    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<()> {
        self.push(value)
    }

    fn end(self) -> Result<Item> {
        seq_to_item(self.items)
    }
}

struct VariantSeqSerializer {
    variant: &'static str,
    inner: SeqSerializer,
}

impl serde::ser::SerializeTupleVariant for VariantSeqSerializer {
    type Ok = Item;
    type Error = Error;

    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<()> {
        self.inner.push(value)
    }

    fn end(self) -> Result<Item> {
        let mut table = Table::new();
        table.insert(self.variant, seq_to_item(self.inner.items)?);
        Ok(Item::Table(table))
    }
}

struct TableSerializer {
    table: Table,
    key: Option<String>,
}

fn insert_unless_none<T: Serialize + ?Sized>(
    table: &mut Table,
    key: &str,
    value: &T,
) -> Result<()> {
    let item = value.serialize(ItemSerializer)?;
    if !item.is_none() {
        table.insert(key, item);
    }
    Ok(())
}

impl serde::ser::SerializeMap for TableSerializer {
    type Ok = Item;
    type Error = Error;

    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<()> {
        self.key = Some(key.serialize(KeySerializer)?);
        Ok(())
    }

    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<()> {
        let key = self.key.take().expect("serde sends a key before its value");
        self.table
            .insert(&key, written(value.serialize(ItemSerializer)?)?);
        Ok(())
    }

    fn serialize_field_entry<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<()> {
        insert_unless_none(&mut self.table, key, value)
    }

    fn end(self) -> Result<Item> {
        Ok(Item::Table(self.table))
    }
}

struct StructSerializer {
    table: Table,
    datetime: bool,
}

impl serde::ser::SerializeStruct for StructSerializer {
    type Ok = Item;
    type Error = Error;

    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<()> {
        if self.datetime && key == DATETIME_FIELD {
            let text = value.serialize(KeySerializer)?;
            let parsed = text
                .parse::<crate::Datetime>()
                .map_err(|e| Error::message(e.to_string()))?;
            self.table
                .insert(DATETIME_FIELD, Item::Value(EValue::from(parsed)));
            return Ok(());
        }
        insert_unless_none(&mut self.table, key, value)
    }

    fn end(self) -> Result<Item> {
        if self.datetime
            && let Some(item) = self.table.get(DATETIME_FIELD)
        {
            return Ok(item.clone());
        }
        Ok(Item::Table(self.table))
    }
}

struct VariantTableSerializer {
    variant: &'static str,
    table: Table,
}

impl serde::ser::SerializeStructVariant for VariantTableSerializer {
    type Ok = Item;
    type Error = Error;

    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<()> {
        insert_unless_none(&mut self.table, key, value)
    }

    fn end(self) -> Result<Item> {
        let mut outer = Table::new();
        outer.insert(self.variant, Item::Table(self.table));
        Ok(Item::Table(outer))
    }
}

struct KeySerializer;

impl serde::Serializer for KeySerializer {
    type Ok = String;
    type Error = Error;
    type SerializeSeq = Impossible<String, Error>;
    type SerializeTuple = Impossible<String, Error>;
    type SerializeTupleStruct = Impossible<String, Error>;
    type SerializeTupleVariant = Impossible<String, Error>;
    type SerializeMap = Impossible<String, Error>;
    type SerializeStruct = Impossible<String, Error>;
    type SerializeStructVariant = Impossible<String, Error>;

    fn serialize_str(self, v: &str) -> Result<String> {
        Ok(v.to_owned())
    }

    fn serialize_char(self, v: char) -> Result<String> {
        Ok(v.to_string())
    }

    fn serialize_bool(self, v: bool) -> Result<String> {
        Ok(v.to_string())
    }

    fn serialize_i8(self, v: i8) -> Result<String> {
        Ok(v.to_string())
    }

    fn serialize_i16(self, v: i16) -> Result<String> {
        Ok(v.to_string())
    }

    fn serialize_i32(self, v: i32) -> Result<String> {
        Ok(v.to_string())
    }

    fn serialize_i64(self, v: i64) -> Result<String> {
        Ok(v.to_string())
    }

    fn serialize_i128(self, v: i128) -> Result<String> {
        Ok(v.to_string())
    }

    fn serialize_u8(self, v: u8) -> Result<String> {
        Ok(v.to_string())
    }

    fn serialize_u16(self, v: u16) -> Result<String> {
        Ok(v.to_string())
    }

    fn serialize_u32(self, v: u32) -> Result<String> {
        Ok(v.to_string())
    }

    fn serialize_u64(self, v: u64) -> Result<String> {
        Ok(v.to_string())
    }

    fn serialize_u128(self, v: u128) -> Result<String> {
        Ok(v.to_string())
    }

    fn serialize_f32(self, _v: f32) -> Result<String> {
        unsupported("a float as a table key")
    }

    fn serialize_f64(self, _v: f64) -> Result<String> {
        unsupported("a float as a table key")
    }

    fn serialize_bytes(self, _v: &[u8]) -> Result<String> {
        unsupported("a byte string as a table key")
    }

    fn serialize_none(self) -> Result<String> {
        unsupported("a null table key")
    }

    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<String> {
        value.serialize(self)
    }

    fn serialize_unit(self) -> Result<String> {
        unsupported("a unit table key")
    }

    fn serialize_unit_struct(self, _name: &'static str) -> Result<String> {
        unsupported("a unit-struct table key")
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
    ) -> Result<String> {
        Ok(variant.to_owned())
    }

    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<String> {
        value.serialize(self)
    }

    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        _value: &T,
    ) -> Result<String> {
        unsupported("a newtype-variant table key")
    }

    fn serialize_seq(self, _len: Option<usize>) -> Result<Self::SerializeSeq> {
        unsupported("a sequence as a table key")
    }

    fn serialize_tuple(self, _len: usize) -> Result<Self::SerializeTuple> {
        unsupported("a tuple as a table key")
    }

    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleStruct> {
        unsupported("a tuple struct as a table key")
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleVariant> {
        unsupported("a tuple variant as a table key")
    }

    fn serialize_map(self, _len: Option<usize>) -> Result<Self::SerializeMap> {
        unsupported("a map as a table key")
    }

    fn serialize_struct(self, _name: &'static str, _len: usize) -> Result<Self::SerializeStruct> {
        unsupported("a struct as a table key")
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStructVariant> {
        unsupported("a struct variant as a table key")
    }
}

impl Serialize for crate::Value {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> core::result::Result<S::Ok, S::Error> {
        match self {
            crate::Value::String(v) => serializer.serialize_str(v),
            crate::Value::Integer(v) => serializer.serialize_i64(*v),
            crate::Value::Float(v) => serializer.serialize_f64(*v),
            crate::Value::Boolean(v) => serializer.serialize_bool(*v),
            crate::Value::Datetime(v) => v.serialize(serializer),
            crate::Value::Array(v) => v.serialize(serializer),
            crate::Value::Table(v) => v.serialize(serializer),
        }
    }
}

impl<K: Ord + Serialize, V: Serialize> Serialize for crate::value::Map<K, V> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> core::result::Result<S::Ok, S::Error> {
        serializer.collect_map(self.iter())
    }
}
