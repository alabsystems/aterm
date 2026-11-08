// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Compiler wire evidence before general-purpose JSON maps can erase ambiguity.

use aterm_json::Value;

/// Keep every valid identity named by an ambiguous message for invocation-local taint.
pub(crate) enum UnitMessage {
    Other,
    Unit(Value),
    Ambiguous {
        identities: Vec<String>,
        /// At least one decoded top-level reason names an artifact, even if reasons conflict.
        artifact_reason_seen: bool,
    },
}

impl<'de> serde::Deserialize<'de> for UnitMessage {
    fn deserialize<D: serde::Deserializer<'de>>(decoder: D) -> Result<Self, D::Error> {
        struct Wire;
        impl<'de> serde::de::Visitor<'de> for Wire {
            type Value = UnitMessage;
            fn expecting(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                out.write_str("a compiler message object")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut input: A,
            ) -> Result<Self::Value, A::Error> {
                let mut fields = aterm_json::Map::new();
                let mut identities = std::collections::BTreeSet::new();
                let mut unit = false;
                let mut artifact_reason_seen = false;
                let mut duplicate = false;
                while let Some((key, checked)) = input.next_entry::<String, CheckedValue>()? {
                    duplicate |= checked.duplicate;
                    let value = checked.value;
                    artifact_reason_seen |=
                        key == "reason" && value.as_str() == Some("compiler-artifact");
                    if key == "reason"
                        && matches!(
                            value.as_str(),
                            Some("compiler-artifact" | "compilation-unit-dispatched")
                        )
                    {
                        unit = true;
                    }
                    if key == "trust_unit_identity_sha256"
                        && let Some(identity) = value
                            .as_str()
                            .filter(|id| aterm_codec::hex::is_lower(id, 64))
                    {
                        identities.insert(identity.to_owned());
                    }
                    match fields.entry(key) {
                        std::collections::btree_map::Entry::Occupied(_) => duplicate = true,
                        std::collections::btree_map::Entry::Vacant(slot) => {
                            slot.insert(value);
                        }
                    }
                }
                Ok(if !unit {
                    UnitMessage::Other
                } else if duplicate {
                    UnitMessage::Ambiguous {
                        identities: identities.into_iter().collect(),
                        artifact_reason_seen,
                    }
                } else {
                    UnitMessage::Unit(Value::Object(fields))
                })
            }
        }
        decoder.deserialize_map(Wire)
    }
}

/// An executable artifact must name whole string kinds, never a lossy projection.
pub(crate) fn artifact_target_kinds(message: &Value) -> Result<Vec<&str>, &'static str> {
    message
        .get("target")
        .and_then(|target| target.get("kind"))
        .and_then(Value::as_array)
        .ok_or("compiler artifact has no target kind array")?
        .iter()
        .map(|kind| {
            kind.as_str()
                .ok_or("compiler artifact has a non-string target kind")
        })
        .collect()
}

/// Uses the JSON decoder's existing depth bound, including objects nested in arrays.
struct CheckedValue {
    value: Value,
    duplicate: bool,
}

impl CheckedValue {
    fn plain(value: Value) -> Self {
        Self {
            value,
            duplicate: false,
        }
    }
}

impl<'de> serde::Deserialize<'de> for CheckedValue {
    fn deserialize<D: serde::Deserializer<'de>>(decoder: D) -> Result<Self, D::Error> {
        struct Checked;
        impl<'de> serde::de::Visitor<'de> for Checked {
            type Value = CheckedValue;
            fn expecting(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                out.write_str("a JSON value")
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(CheckedValue::plain(Value::Null))
            }
            fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<Self::Value, E> {
                Ok(CheckedValue::plain(Value::from(value)))
            }
            fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Self::Value, E> {
                Ok(CheckedValue::plain(Value::from(value)))
            }
            fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Self::Value, E> {
                Ok(CheckedValue::plain(Value::from(value)))
            }
            fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Self::Value, E> {
                Ok(CheckedValue::plain(Value::from(value)))
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(CheckedValue::plain(Value::from(value)))
            }
            fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Self::Value, E> {
                Ok(CheckedValue::plain(Value::from(value)))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut input: A,
            ) -> Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                let mut duplicate = false;
                while let Some(checked) = input.next_element::<CheckedValue>()? {
                    duplicate |= checked.duplicate;
                    values.push(checked.value);
                }
                Ok(CheckedValue {
                    value: Value::Array(values),
                    duplicate,
                })
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut input: A,
            ) -> Result<Self::Value, A::Error> {
                let mut values = aterm_json::Map::new();
                let mut duplicate = false;
                while let Some((key, checked)) = input.next_entry::<String, CheckedValue>()? {
                    duplicate |= checked.duplicate;
                    match values.entry(key) {
                        std::collections::btree_map::Entry::Occupied(_) => duplicate = true,
                        std::collections::btree_map::Entry::Vacant(slot) => {
                            slot.insert(checked.value);
                        }
                    }
                }
                Ok(CheckedValue {
                    value: Value::Object(values),
                    duplicate,
                })
            }
        }
        decoder.deserialize_any(Checked)
    }
}
