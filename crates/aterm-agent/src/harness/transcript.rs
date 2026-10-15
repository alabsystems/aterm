// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Metadata-only transcript reads. The whole row is still validated, including
//! discarded content, but a large tool input/result never becomes a Value tree.
//! Keep Value's tolerant types and last-key-wins rule: vendor fields with the
//! wrong shape read as absent, rather than invalidating unrelated metadata.

use std::fmt;

use aterm_json::{Map, Value};
use serde::de::{DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};

#[derive(Clone, Copy)]
enum Shape {
    Scalar,
    Object(&'static [Field]),
    Content,
}

type Field = (&'static str, Shape);

const USAGE: Shape = Shape::Object(&[
    ("type", Shape::Scalar),
    ("subtype", Shape::Scalar),
    ("durationMs", Shape::Scalar),
    ("isSidechain", Shape::Scalar),
    ("isApiErrorMessage", Shape::Scalar),
    ("timestamp", Shape::Scalar),
    (
        "message",
        Shape::Object(&[
            ("id", Shape::Scalar),
            ("model", Shape::Scalar),
            (
                "usage",
                Shape::Object(&[
                    ("input_tokens", Shape::Scalar),
                    ("output_tokens", Shape::Scalar),
                    ("cache_creation_input_tokens", Shape::Scalar),
                    ("cache_read_input_tokens", Shape::Scalar),
                ]),
            ),
        ]),
    ),
]);

const METADATA_FIELDS: [Field; 7] = [
    ("type", Shape::Scalar),
    ("isSidechain", Shape::Scalar),
    ("timestamp", Shape::Scalar),
    ("version", Shape::Scalar),
    ("cwd", Shape::Scalar),
    ("effort", Shape::Scalar),
    ("message", Shape::Object(&[("model", Shape::Scalar)])),
];
const METADATA: Shape = Shape::Object(&METADATA_FIELDS);
const COMMAND_FIELDS: [Field; 7] = {
    let mut fields = METADATA_FIELDS;
    fields[6] = (
        "message",
        Shape::Object(&[("model", Shape::Scalar), ("content", Shape::Scalar)]),
    );
    fields
};
const COMMAND: Shape = Shape::Object(&COMMAND_FIELDS);
const CONTENT_PART: Shape = Shape::Object(&[("type", Shape::Scalar), ("text", Shape::Scalar)]);
/// What is kept of a limit row's `quotaLimits`: whether the request was
/// rejected, the reset it names, and which of the account's limits it was.
/// MEASURED on this machine's transcripts (2026-10-01: every one of the ten
/// rows that carry the key, Claude Code 2.1.273 and 2.1.283; 2.1.286's row
/// is the accounts design §1's): `{"status":"rejected","resetsAt":<unix s,
/// an integer>,"unifiedRateLimitFallbackAvailable":false,
/// "rateLimitType":"five_hour" | "seven_day","overageStatus":…,
/// "overageDisabledReason":…,"isUsingOverage":false}`, five of 2.1.273's
/// with three `lowPriority…` keys besides. None of those other keys is read
/// by anything here, and none is kept.
const QUOTA_LIMITS: Shape = Shape::Object(&[
    ("status", Shape::Scalar),
    ("resetsAt", Shape::Scalar),
    ("rateLimitType", Shape::Scalar),
]);
const CONVERSATION: Shape = Shape::Object(&[
    ("type", Shape::Scalar),
    // `null` on a conversation's first row: the supervisor's wall check
    // reads a first prompt written after the rows that answer it
    // (`supervise/wall_confirm.rs`).
    ("parentUuid", Shape::Scalar),
    // The row's own id: a usage limit's notice confirmed is remembered by
    // it, so only a reply written after THAT row ends the wait on it
    // (`supervise/wall_confirm.rs`'s `served_since_notice`).
    ("uuid", Shape::Scalar),
    ("isSidechain", Shape::Scalar),
    ("isMeta", Shape::Scalar),
    ("isCompactSummary", Shape::Scalar),
    ("isApiErrorMessage", Shape::Scalar),
    ("error", Shape::Scalar),
    ("timestamp", Shape::Scalar),
    ("subtype", Shape::Scalar),
    ("content", Shape::Scalar),
    ("quotaLimits", QUOTA_LIMITS),
    (
        "message",
        Shape::Object(&[("model", Shape::Scalar), ("content", Shape::Content)]),
    ),
]);

/// The Codex turn/head and sandbox readers use only these discriminants and
/// context facts. Instructions, environment state, tool payloads and messages
/// are still validated, but never materialized merely to identify their row.
const CODEX_METADATA: Shape = Shape::Object(&[
    ("type", Shape::Scalar),
    ("timestamp", Shape::Scalar),
    (
        "payload",
        Shape::Object(&[
            ("type", Shape::Scalar),
            ("role", Shape::Scalar),
            ("cwd", Shape::Scalar),
            ("model", Shape::Scalar),
            ("sandbox_policy", Shape::Object(&[("type", Shape::Scalar)])),
        ]),
    ),
]);

struct CodexMetadata(Value);

impl<'de> Deserialize<'de> for CodexMetadata {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        CODEX_METADATA.deserialize(de).map(Self)
    }
}

pub(crate) fn codex_metadata(line: &str) -> aterm_json::Result<Value> {
    aterm_json::from_str::<CodexMetadata>(line).map(|v| v.0)
}

struct Usage(Value);
struct Metadata<const CONTENT: bool>(Value);
struct Conversation(Value);

impl<'de> Deserialize<'de> for Usage {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        USAGE.deserialize(de).map(Self)
    }
}

impl<'de, const CONTENT: bool> Deserialize<'de> for Metadata<CONTENT> {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let shape = if CONTENT { COMMAND } else { METADATA };
        shape.deserialize(de).map(Self)
    }
}

impl<'de> Deserialize<'de> for Conversation {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        CONVERSATION.deserialize(de).map(Self)
    }
}

pub(super) fn usage(line: &str) -> aterm_json::Result<Value> {
    aterm_json::from_str::<Usage>(line).map(|v| v.0)
}

pub(super) fn metadata(line: &str) -> aterm_json::Result<Value> {
    aterm_json::from_str::<Metadata<false>>(line).map(|v| v.0)
}

/// A `/model` command additionally needs its plain-string content. Block
/// arrays (tool calls/results) still need no materialized content.
pub(super) fn command(line: &str) -> aterm_json::Result<Value> {
    aterm_json::from_str::<Metadata<true>>(line).map(|v| v.0)
}

/// The upgrade/task readers additionally need text blocks and the vendor's
/// own error/limit metadata, but never tool arguments, images, or results.
pub(crate) fn conversation(line: &str) -> aterm_json::Result<Value> {
    aterm_json::from_str::<Conversation>(line).map(|v| v.0)
}

impl<'de> DeserializeSeed<'de> for Shape {
    type Value = Value;

    fn deserialize<D: Deserializer<'de>>(self, de: D) -> Result<Value, D::Error> {
        de.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Shape {
    type Value = Value;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a transcript value")
    }

    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_bool<E>(self, v: bool) -> Result<Value, E> {
        Ok(Value::Bool(v))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Value, E> {
        Ok(Value::from(v))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Value, E> {
        Ok(Value::from(v))
    }

    fn visit_f64<E>(self, v: f64) -> Result<Value, E> {
        Ok(Value::from(v))
    }

    fn visit_str<E>(self, v: &str) -> Result<Value, E> {
        Ok(Value::String(v.to_owned()))
    }

    fn visit_string<E>(self, v: String) -> Result<Value, E> {
        Ok(Value::String(v))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        if matches!(self, Shape::Content) {
            let mut parts = Vec::new();
            while let Some(part) = seq.next_element_seed(CONTENT_PART)? {
                parts.push(part);
            }
            return Ok(Value::Array(parts));
        }
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(Value::Null)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let Shape::Object(fields) = self else {
            while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
            return Ok(Value::Null);
        };
        let mut kept = Map::new();
        while let Some(field) = map.next_key_seed(FieldName(fields))? {
            if let Some((name, shape)) = field {
                kept.insert(name.to_owned(), map.next_value_seed(shape)?);
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(Value::Object(kept))
    }
}

/// Resolve a key without allocating a String for every discarded field.
struct FieldName(&'static [Field]);

impl<'de> DeserializeSeed<'de> for FieldName {
    type Value = Option<Field>;

    fn deserialize<D: Deserializer<'de>>(self, de: D) -> Result<Self::Value, D::Error> {
        de.deserialize_identifier(self)
    }
}

impl<'de> Visitor<'de> for FieldName {
    type Value = Option<Field>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a transcript field name")
    }

    fn visit_str<E>(self, key: &str) -> Result<Self::Value, E> {
        Ok(self.0.iter().find(|(name, _)| *name == key).copied())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(value: Value, shape: Shape) -> Value {
        match (value, shape) {
            (Value::Array(parts), Shape::Content) => Value::Array(
                parts
                    .into_iter()
                    .map(|p| project(p, CONTENT_PART))
                    .collect(),
            ),
            (Value::Object(mut obj), Shape::Object(fields)) => Value::Object(
                fields
                    .iter()
                    .filter_map(|&(name, shape)| {
                        obj.remove(name)
                            .map(|v| (name.to_owned(), project(v, shape)))
                    })
                    .collect(),
            ),
            (Value::Object(_) | Value::Array(_), _) => Value::Null,
            (scalar, _) => scalar,
        }
    }

    #[test]
    fn projections_match_full_values_including_vendor_type_drift_and_duplicates() {
        for value in [
            "null",
            "true",
            "false",
            "0",
            "-1",
            "-0",
            "1.5",
            "1e3",
            "[]",
            "{}",
            r#""text\n\uD83D\uDE80""#,
            r#"{"model":"opus","content":[{"type":"tool_use","input":{"rows":[1,2]}}],"usage":{"input_tokens":42}}"#,
            r#"{"model":"opus","content":[{"type":"tool_result","content":[{"text":"not a prompt"}]},null,12,"scalar",["nested"],{"type":"text","text":"first"},{"type":"text","text":"second"}]}"#,
        ] {
            for text in [
                value.to_owned(),
                format!(r#"{{"type":{value},"payload":{value}}}"#),
                format!(
                    r#"{{"payload":{{"type":{value},"role":{value},"cwd":{value},"model":{value},"sandbox_policy":{{"type":{value}}}}}}}"#
                ),
                format!(
                    r#"{{"type":"ignored","t\u0079pe":"turn_context","payload":null,"payload":{{"cwd":"/old","c\u0077d":{value},"sandbox_policy":{{"type":"read-only","type":{value}}}}}}}"#
                ),
                format!(
                    r#"{{"message":{value},"effort":{value},"cwd":{value},"isApiErrorMessage":{value},"timestamp":{value}}}"#
                ),
                format!(r#"{{"message":{{"model":{value},"usage":{value},"content":{value}}}}}"#),
                format!(
                    r#"{{"type":"ignored","t\u0079pe":"assistant","message":null,"message":{{"model":"opus","usage":{{"input_tokens":3,"input_tokens":{value}}}}}}}"#
                ),
            ] {
                let full: Value = aterm_json::from_str(&text).expect("fixture");
                assert_eq!(usage(&text).expect("usage"), project(full.clone(), USAGE));
                assert_eq!(
                    metadata(&text).expect("metadata"),
                    project(full.clone(), METADATA)
                );
                assert_eq!(
                    command(&text).expect("command"),
                    project(full.clone(), COMMAND)
                );
                assert_eq!(
                    conversation(&text).expect("conversation"),
                    project(full.clone(), CONVERSATION)
                );
                assert_eq!(
                    codex_metadata(&text).expect("Codex metadata"),
                    project(full, CODEX_METADATA)
                );
            }
        }
    }

    /// THE LIMIT ROW, as Claude Code 2.1.286 writes it (the accounts design
    /// §1's row; ids cut): the conversation projection keeps the three keys
    /// of `quotaLimits` its readers ask for — `status`, `resetsAt`,
    /// `rateLimitType` — and none of the overage keys; the other projections
    /// keep no `quotaLimits` at all. A key of the wrong shape reads as the
    /// null it stands for, and does not cost the row its other keys.
    #[test]
    fn the_conversation_keeps_a_limit_rows_status_reset_and_limit_type() {
        let row = r#"{"type":"assistant","isApiErrorMessage":true,"error":"rate_limit","timestamp":"2026-09-30T19:02:11.123Z","quotaLimits":{"status":"rejected","resetsAt":1790870400,"unifiedRateLimitFallbackAvailable":false,"rateLimitType":"seven_day","overageStatus":"rejected","overageDisabledReason":"org_level_disabled","isUsingOverage":false},"message":{"model":"<synthetic>","content":[{"type":"text","text":"You've hit your weekly limit · resets Oct 1 at 9am (America/Los_Angeles)"}]}}"#;
        let kept = conversation(row).expect("a row");
        let quota = kept.get("quotaLimits").expect("quotaLimits");
        assert_eq!(
            aterm_json::to_string(quota).expect("json"),
            r#"{"rateLimitType":"seven_day","resetsAt":1790870400,"status":"rejected"}"#
        );
        // NEGATIVE CONTROL: the projections that read no limit keep none.
        for other in [usage(row), metadata(row), command(row)] {
            assert_eq!(other.expect("a row").get("quotaLimits"), None);
        }
        // Vendor type drift: each key of the wrong shape is null, the others
        // are kept.
        let drift = row
            .replace(r#""status":"rejected""#, r#""status":{"is":"rejected"}"#)
            .replace(
                r#""rateLimitType":"seven_day""#,
                r#""rateLimitType":["seven_day"]"#,
            );
        let kept = conversation(&drift).expect("a row");
        let quota = kept.get("quotaLimits").expect("quotaLimits");
        assert_eq!(quota.get("status"), Some(&Value::Null));
        assert_eq!(quota.get("rateLimitType"), Some(&Value::Null));
        assert_eq!(
            quota.get("resetsAt").and_then(Value::as_u64),
            Some(1_790_870_400)
        );
    }

    #[test]
    fn discarded_fields_are_still_fully_validated() {
        for bad in [
            r#""\uD800""#,
            r#""\x""#,
            "[1,]",
            r#"{"a":1,}"#,
            "01",
            "1e9999",
            "true false",
        ] {
            for name in ["unknown", "message"] {
                let text = format!(r#"{{"type":"assistant","{name}":{bad}}}"#);
                assert!(usage(&text).is_err(), "{text}");
                assert!(metadata(&text).is_err(), "{text}");
                assert!(conversation(&text).is_err(), "{text}");
                assert!(codex_metadata(&text).is_err(), "{text}");
            }
        }
        let deep = format!("{}0{}", "[".repeat(130), "]".repeat(130));
        assert!(metadata(&format!(r#"{{"unknown":{deep}}}"#)).is_err());
    }
}
