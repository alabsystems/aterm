// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Shared strict receipt parsing for explicitly selected raw SDK components.

use std::collections::BTreeSet;
use std::path::{Component, Path};

use aterm_json::{Map, Value as V};
use serde::de::{self, Deserialize, Deserializer, Visitor};

pub(super) const LIMIT: usize = 32 << 20;
type R<T> = Result<T, String>;

pub(super) fn need(condition: bool, why: &str) -> R<()> {
    if condition {
        Ok(())
    } else {
        Err(why.to_owned())
    }
}

pub(super) fn fields(value: &V, names: &str) -> R<()> {
    let object = value.as_object().ok_or("expected component object")?;
    let expected: BTreeSet<_> = names.split_whitespace().collect();
    need(
        object.len() == expected.len() && object.keys().all(|k| expected.contains(k.as_str())),
        "component fields differ",
    )
}

pub(super) fn text<'a>(value: &'a V, name: &str) -> R<&'a str> {
    value[name]
        .as_str()
        .ok_or_else(|| format!("component string absent: {name}"))
}

pub(super) fn digest(value: &V) -> R<&str> {
    let value = value.as_str().ok_or("component digest is not a string")?;
    need(
        aterm_codec::hex::is_lower(value, 64),
        "invalid component digest",
    )?;
    Ok(value)
}

pub(super) fn oid(value: &V) -> R<&str> {
    let value = value.as_str().ok_or("component commit is not a string")?;
    need(
        aterm_codec::hex::is_lower(value, 40),
        "invalid component commit",
    )?;
    Ok(value)
}

pub(super) fn absolute(value: &V) -> R<&Path> {
    let path = Path::new(value.as_str().ok_or("component path is not a string")?);
    need(
        path.is_absolute()
            && !path
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::CurDir)),
        "component path is not absolute without traversal",
    )?;
    Ok(path)
}

pub(super) fn reference(value: &V) -> R<&str> {
    fields(value, "path sha256")?;
    absolute(&value["path"])?;
    digest(&value["sha256"])
}

pub(super) fn hash(raw: &[u8]) -> String {
    aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(raw))
}

pub(super) fn bytes(path: &Path, limit: usize) -> R<Vec<u8>> {
    crate::metadata_io::read_bounded_regular(path, limit)
        .map_err(|e| format!("component file {}: {e}", path.display()))
}

// Value's general map reader permits duplicates. The closed receipt contract
// requires Python's unique-pairs semantics recursively, including doctor data.
pub(super) struct Unique(pub(super) V);

impl<'de> Deserialize<'de> for Unique {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Read;
        impl<'de> Visitor<'de> for Read {
            type Value = Unique;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("JSON without duplicate object keys")
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Unique, E> {
                Ok(Unique(V::from(v)))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Unique, E> {
                Ok(Unique(V::from(v)))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Unique, E> {
                Ok(Unique(V::from(v)))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Unique, E> {
                Ok(Unique(V::from(v)))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Unique, E> {
                Ok(Unique(V::from(v)))
            }
            fn visit_string<E: de::Error>(self, v: String) -> Result<Unique, E> {
                Ok(Unique(V::from(v)))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Unique, E> {
                Ok(Unique(V::Null))
            }
            fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Unique, A::Error> {
                let mut values = Vec::new();
                while let Some(Unique(v)) = seq.next_element()? {
                    values.push(v);
                }
                Ok(Unique(V::Array(values)))
            }
            fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<Unique, A::Error> {
                let mut values = Map::new();
                while let Some((key, Unique(value))) = map.next_entry::<String, Unique>()? {
                    if values.insert(key, value).is_some() {
                        return Err(de::Error::custom("duplicate component field"));
                    }
                }
                Ok(Unique(V::Object(values)))
            }
        }
        deserializer.deserialize_any(Read)
    }
}

pub(super) fn document(path: &Path, expected: Option<&str>) -> R<V> {
    let raw = bytes(path, LIMIT)?;
    if let Some(expected) = expected {
        need(hash(&raw) == expected, "component evidence digest differs")?;
    }
    aterm_json::from_slice::<Unique>(&raw)
        .map(|value| value.0)
        .map_err(|e| format!("component JSON: {e}"))
}
