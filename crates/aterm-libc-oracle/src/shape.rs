// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE EMITTED-SOURCE SHAPE GATE: what a type-level oracle cannot see.

use std::collections::BTreeSet;
use std::fmt;

use crate::closure::{PRIMS, closure};
use crate::rustdoc::{Doc, Entry, FormatError, Namespace, Public, Result, kind_of};
use aterm_json::{Map, Value};

const PUBLIC_TRAITS: [&str; 6] = ["Clone", "Copy", "Debug", "PartialEq", "Eq", "Hash"];

/// One fact about a crate's public surface.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ShapeRecord {
    Item {
        name: String,
        namespace: Namespace,
        kind: String,
    },
    Body {
        name: String,
        has_body: bool,
    },
    /// The function's whole rustdoc header (const/unsafe/async/ABI), as compact sorted JSON.
    Header {
        name: String,
        header: String,
    },
    Field {
        owner: String,
        field: String,
    },
    Variant {
        owner: String,
        variant: String,
    },
    Inherent {
        owner: String,
        name: String,
        kind: String,
    },
    InherentBody {
        owner: String,
        name: String,
        has_body: bool,
    },
    InherentHeader {
        owner: String,
        name: String,
        header: String,
    },
    Trait {
        owner: String,
        kind: String,
        /// The trait's canonical path.
        path: String,
        /// The normalized generic arguments as compact JSON, `None` for none.
        args: Option<String>,
        negative: bool,
    },
}

impl fmt::Display for ShapeRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Item {
                name,
                namespace,
                kind,
            } => write!(f, "{name}: public {} item is {kind}", namespace.as_str()),
            Self::Body { name, has_body } => write!(
                f,
                "{name}: function is {}",
                if *has_body {
                    "a Rust body"
                } else {
                    "an extern declaration"
                }
            ),
            Self::Header { name, header } => write!(f, "{name}: function header is {header}"),
            Self::Field { owner, field } => write!(f, "{owner}.{field}: public field"),
            Self::Variant { owner, variant } => write!(f, "{owner}::{variant}: public variant"),
            Self::Inherent { owner, name, kind } => {
                write!(f, "{owner}::{name}: public inherent {kind}")
            }
            Self::InherentBody {
                owner,
                name,
                has_body,
            } => write!(
                f,
                "{owner}::{name}: inherent function is {}",
                if *has_body {
                    "a Rust body"
                } else {
                    "a declaration"
                }
            ),
            Self::InherentHeader {
                owner,
                name,
                header,
            } => write!(f, "{owner}::{name}: inherent function header is {header}"),
            Self::Trait {
                owner,
                path,
                args,
                negative,
                ..
            } => {
                let polarity = if *negative {
                    "negative impl !"
                } else {
                    "explicit trait impl "
                };
                write!(f, "{owner}: {polarity}{path}")?;
                match args {
                    Some(args) => write!(f, " with args {args}"),
                    None => Ok(()),
                }
            }
        }
    }
}

/// Which side of the comparison a record is on alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Side {
    /// The reference declares it; the shim does not.
    Missing,
    /// The shim declares it; the reference does not.
    Unexpected,
}

/// One record the two surfaces disagree on.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ShapeDiff {
    pub side: Side,
    pub record: ShapeRecord,
}

impl fmt::Display for ShapeDiff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let side = match self.side {
            Side::Missing => "missing",
            Side::Unexpected => "unexpected",
        };
        write!(f, "{side} {}", self.record)
    }
}

/// A function item's `has_body`: false for an `extern` declaration.
/// # Errors
/// When rustdoc omitted the bit.
pub fn function_has_body(name: &str, item: &Value) -> Result<bool> {
    item.get("inner")
        .and_then(|inner| inner.get("function")?.get("has_body")?.as_bool())
        .ok_or_else(|| FormatError(format!("rustdoc omitted function.has_body for {name}")))
}

/// A function item's whole header as compact sorted JSON.
/// # Errors
/// When rustdoc omitted the header.
pub fn function_header(name: &str, item: &Value) -> Result<String> {
    item.get("inner")
        .and_then(|inner| inner.get("function")?.get("header"))
        .filter(|header| header.is_object())
        .map(ToString::to_string)
        .ok_or_else(|| FormatError(format!("rustdoc omitted function.header for {name}")))
}

fn canonical_path(path: &Value, doc: &Doc) -> String {
    let fallback = || {
        path.get("path")
            .and_then(Value::as_str)
            .unwrap_or("<unnamed>")
            .to_string()
    };
    let Some(summary) = path.get("id").and_then(|id| doc.path_summary(id)) else {
        return fallback();
    };
    let mut segments: Vec<&str> = summary
        .get("path")
        .and_then(Value::as_array)
        .map(|s| s.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if segments.is_empty() {
        return fallback();
    }
    if summary.get("crate_id").and_then(Value::as_u64) == Some(0) {
        segments[0] = "$crate";
    }
    segments.join("::")
}

fn normalize(value: &Value, doc: &Doc) -> Value {
    match value {
        Value::Object(map)
            if map.get("path").and_then(Value::as_str).is_some() && map.contains_key("id") =>
        {
            let mut out = Map::new();
            out.insert("path".into(), Value::String(canonical_path(value, doc)));
            out.insert(
                "args".into(),
                normalize(map.get("args").unwrap_or(&Value::Null), doc),
            );
            for (key, item) in map {
                if !matches!(key.as_str(), "path" | "id" | "args") {
                    out.insert(key.clone(), normalize(item, doc));
                }
            }
            Value::Object(out)
        }
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, item)| (key.clone(), normalize(item, doc)))
                .collect(),
        ),
        Value::Array(items) => {
            Value::Array(items.iter().map(|item| normalize(item, doc)).collect())
        }
        other => other.clone(),
    }
}

fn trait_records(
    owner: &str,
    kind: &str,
    inner: &Value,
    doc: &Doc,
    out: &mut BTreeSet<ShapeRecord>,
) {
    for (imp, tr) in doc.declared_trait_impls(inner) {
        let path = canonical_path(tr, doc);
        let last = path.rsplit("::").next().unwrap_or_default();
        if !PUBLIC_TRAITS.contains(&last) {
            continue;
        }
        let args = normalize(tr.get("args").unwrap_or(&Value::Null), doc);
        out.insert(ShapeRecord::Trait {
            owner: owner.into(),
            kind: kind.into(),
            path,
            args: (!args.is_null()).then(|| args.to_string()),
            negative: imp.get("is_negative").and_then(Value::as_bool) == Some(true),
        });
    }
}

fn inherent_records(
    owner: &str,
    inner: &Value,
    doc: &Doc,
    out: &mut BTreeSet<ShapeRecord>,
) -> Result<()> {
    for child in doc.public_inherent_items(inner) {
        let name = child
            .get("name")
            .and_then(Value::as_str)
            .filter(|n| !n.is_empty());
        let (Some(name), Some((kind, _))) = (name, kind_of(child)) else {
            return Err(FormatError(format!(
                "malformed public inherent item on {owner}"
            )));
        };
        out.insert(ShapeRecord::Inherent {
            owner: owner.into(),
            name: name.into(),
            kind: kind.into(),
        });
        if kind == "function" {
            let full = format!("{owner}::{name}");
            out.insert(ShapeRecord::InherentBody {
                owner: owner.into(),
                name: name.into(),
                has_body: function_has_body(&full, child)?,
            });
            out.insert(ShapeRecord::InherentHeader {
                owner: owner.into(),
                name: name.into(),
                header: function_header(&full, child)?,
            });
        }
    }
    Ok(())
}

fn member_names(owner: &str, ids: Option<&Value>, what: &str, doc: &Doc) -> Result<Vec<String>> {
    ids.and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|id| {
            doc.item(id)
                .and_then(|member| member.get("name")?.as_str())
                .filter(|name| !name.is_empty())
                .map(str::to_string)
                .ok_or_else(|| FormatError(format!("rustdoc omitted a public {what} of {owner}")))
        })
        .collect()
}

fn entry_records(
    name: &str,
    namespace: Namespace,
    entry: &Entry<'_>,
    doc: &Doc,
    out: &mut BTreeSet<ShapeRecord>,
) -> Result<()> {
    out.insert(ShapeRecord::Item {
        name: name.into(),
        namespace,
        kind: entry.kind.into(),
    });
    let inner = entry.inner()?;
    match entry.kind {
        "function" => {
            out.insert(ShapeRecord::Body {
                name: name.into(),
                has_body: function_has_body(name, entry.item)?,
            });
            out.insert(ShapeRecord::Header {
                name: name.into(),
                header: function_header(name, entry.item)?,
            });
        }
        "struct" | "union" | "enum" => {
            let (members, what) = match entry.kind {
                "struct" => {
                    let kind = inner.get("kind").ok_or_else(|| {
                        FormatError(format!("rustdoc omitted struct.kind of {name}"))
                    })?;
                    (
                        kind.get("plain").and_then(|plain| plain.get("fields")),
                        "field",
                    )
                }
                "union" => (inner.get("fields"), "field"),
                _ => (inner.get("variants"), "variant"),
            };
            for member in member_names(name, members, what, doc)? {
                out.insert(if what == "field" {
                    ShapeRecord::Field {
                        owner: name.into(),
                        field: member,
                    }
                } else {
                    ShapeRecord::Variant {
                        owner: name.into(),
                        variant: member,
                    }
                });
            }
            inherent_records(name, inner, doc, out)?;
            trait_records(name, entry.kind, inner, doc, out);
        }
        _ => {}
    }
    Ok(())
}

/// The surface `public` shows, restricted to `names` when given.
/// # Errors
/// When rustdoc omitted a fact a record needs.
pub fn public_shape(
    doc: &Doc,
    public: &Public<'_>,
    names: Option<&BTreeSet<String>>,
) -> Result<BTreeSet<ShapeRecord>> {
    let mut out = BTreeSet::new();
    for (name, by_namespace) in public {
        if names.is_some_and(|names| !names.contains(*name)) {
            continue;
        }
        for (namespace, entry) in by_namespace {
            entry_records(name, *namespace, entry, doc, &mut out)?;
        }
    }
    Ok(out)
}

/// The reference's surface over one cell's declaration set.
/// # Errors
/// When the reference's rustdoc omitted a fact a record needs.
pub fn expected_shape(
    reference: &Doc,
    public: &Public<'_>,
    union_names: &[String],
) -> Result<BTreeSet<ShapeRecord>> {
    let mut names = closure(reference, public, union_names)?;
    names.extend(PRIMS.iter().map(|p| (*p).to_string()));
    public_shape(reference, public, Some(&names))
}

/// Every record on exactly one side.
#[must_use]
pub fn diff(expected: &BTreeSet<ShapeRecord>, actual: &BTreeSet<ShapeRecord>) -> Vec<ShapeDiff> {
    let side = |side, records: &BTreeSet<ShapeRecord>, other: &BTreeSet<ShapeRecord>| {
        records
            .difference(other)
            .map(move |record| ShapeDiff {
                side,
                record: record.clone(),
            })
            .collect::<Vec<_>>()
    };
    let mut diffs = side(Side::Missing, expected, actual);
    diffs.extend(side(Side::Unexpected, actual, expected));
    diffs
}

/// How many public items a surface has.
#[must_use]
pub fn item_count(shape: &BTreeSet<ShapeRecord>) -> usize {
    shape
        .iter()
        .filter(|r| matches!(r, ShapeRecord::Item { .. }))
        .count()
}

#[cfg(test)]
#[path = "shape_tests.rs"]
mod tests;
