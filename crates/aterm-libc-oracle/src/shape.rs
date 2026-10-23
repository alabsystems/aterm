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
mod tests {
    use super::*;
    use crate::rustdoc::fixture::{doc, function};

    fn shape_of(text: &str) -> BTreeSet<ShapeRecord> {
        let parsed = Doc::parse(text).expect("parses");
        let public = parsed.public().expect("public");
        public_shape(&parsed, &public, None).expect("shape")
    }

    fn struct_doc(method_unsafe: bool, with_clone: bool) -> String {
        let impls = if with_clone { "4,5,6,7" } else { "5,6,7" };
        let index = [
            format!(
                r#""2":{{"id":2,"name":"timespec","visibility":"public","inner":{{"struct":{{"kind":{{"plain":{{"fields":[3]}}}},"impls":[{impls}]}}}}}}"#
            ),
            r#""3":{"id":3,"name":"tv_sec","inner":{"struct_field":{"primitive":"i64"}}}"#.into(),
            r#""4":{"id":4,"inner":{"impl":{"trait":{"path":"Clone","id":90,"args":null},"items":[],"is_synthetic":false,"blanket_impl":null,"is_negative":false}}}"#.into(),
            r#""5":{"id":5,"inner":{"impl":{"trait":{"path":"Send","id":91,"args":null},"items":[],"is_synthetic":true,"blanket_impl":null,"is_negative":false}}}"#.into(),
            r#""6":{"id":6,"inner":{"impl":{"trait":{"path":"Debug","id":92,"args":null},"items":[],"is_synthetic":false,"blanket_impl":{"generic":"T"},"is_negative":false}}}"#.into(),
            r#""7":{"id":7,"inner":{"impl":{"trait":null,"items":[8],"is_synthetic":false,"blanket_impl":null}}}"#.into(),
            function(8, "si_pid", true, method_unsafe, ""),
        ]
        .join(",");
        let paths = r#""90":{"crate_id":1,"path":["core","clone","Clone"],"kind":"trait"},
                       "2":{"crate_id":0,"path":["libc","timespec"],"kind":"struct"}"#;
        doc(&[2], &index, paths)
    }

    #[test]
    fn a_struct_shows_its_fields_inherent_items_and_declared_traits_only() {
        let records: Vec<String> = shape_of(&struct_doc(true, true))
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            records,
            [
                "timespec: public type item is struct",
                "timespec.tv_sec: public field",
                "timespec::si_pid: public inherent function",
                "timespec::si_pid: inherent function is a Rust body",
                r#"timespec::si_pid: inherent function header is {"abi":{"C":{"unwind":false}},"is_async":false,"is_const":false,"is_unsafe":true}"#,
                "timespec: explicit trait impl core::clone::Clone",
            ]
        );
    }

    #[test]
    fn a_dropped_trait_and_a_widened_method_are_diffs() {
        let reference = shape_of(&struct_doc(true, true));
        let shim = shape_of(&struct_doc(false, false));
        let diffs = diff(&reference, &shim);
        let clone = ShapeRecord::Trait {
            owner: "timespec".into(),
            kind: "struct".into(),
            path: "core::clone::Clone".into(),
            args: None,
            negative: false,
        };
        assert!(diffs.contains(&ShapeDiff {
            side: Side::Missing,
            record: clone
        }));
        let safety: Vec<(Side, bool)> = diffs
            .iter()
            .filter_map(|d| match &d.record {
                ShapeRecord::InherentHeader { name, header, .. } if name == "si_pid" => {
                    Some((d.side, header.contains(r#""is_unsafe":true"#)))
                }
                _ => None,
            })
            .collect();
        assert_eq!(safety, [(Side::Missing, true), (Side::Unexpected, false)]);
        assert_eq!(diffs.len(), 3, "{diffs:#?}");
    }

    #[test]
    fn an_extern_declaration_and_an_inline_body_differ() {
        let declared = shape_of(&doc(&[1], &function(1, "strlen", false, true, ""), ""));
        let inline = shape_of(&doc(&[1], &function(1, "strlen", true, true, ""), ""));
        let diffs: Vec<String> = diff(&declared, &inline)
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            diffs,
            [
                "missing strlen: function is an extern declaration",
                "unexpected strlen: function is a Rust body",
            ]
        );
    }

    #[test]
    fn a_crate_local_trait_argument_reads_as_dollar_crate_on_both_sides() {
        let with_arg = |local_id: u64, krate: &str| {
            let index = format!(
                r#""2":{{"id":2,"name":"timespec","inner":{{"struct":{{"kind":{{"plain":{{"fields":[]}}}},"impls":[4]}}}}}},
                   "4":{{"id":4,"inner":{{"impl":{{"trait":{{"path":"PartialEq","id":93,"args":{{"angle_bracketed":{{"args":[{{"type":{{"resolved_path":{{"path":"timespec","id":{local_id},"args":null}}}}}}],"constraints":[]}}}}}},"items":[],"is_synthetic":false,"blanket_impl":null,"is_negative":false}}}}}}"#
            );
            let paths = format!(
                r#""93":{{"crate_id":1,"path":["core","cmp","PartialEq"],"kind":"trait"}},
                   "{local_id}":{{"crate_id":0,"path":["{krate}","timespec"],"kind":"struct"}}"#
            );
            shape_of(&doc(&[2], &index, &paths))
        };
        let reference = with_arg(2, "libc");
        let shim = with_arg(77, "libc_shim");
        assert!(diff(&reference, &shim).is_empty());
        let args: Vec<Option<String>> = reference
            .iter()
            .filter_map(|r| match r {
                ShapeRecord::Trait { args, .. } => Some(args.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            args,
            [Some(
                r#"{"angle_bracketed":{"args":[{"type":{"resolved_path":{"args":null,"path":"$crate::timespec"}}}],"constraints":[]}}"#
                    .to_string()
            )]
        );
    }

    #[test]
    fn a_function_without_its_body_bit_is_a_format_error() {
        let index = r#""1":{"id":1,"name":"f","inner":{"function":{"header":{}}}}"#;
        let parsed = Doc::parse(&doc(&[1], index, "")).expect("parses");
        let public = parsed.public().expect("public");
        assert!(public_shape(&parsed, &public, None).is_err());
    }

    #[test]
    fn the_expected_surface_is_the_closure_plus_the_primitives_and_nothing_else() {
        let index = [
            function(1, "strlen", false, true, ""),
            function(2, "unlisted", false, true, ""),
            r#""3":{"id":3,"name":"c_int","inner":{"type_alias":{"type":{"primitive":"i32"}}}}"#
                .into(),
        ]
        .join(",");
        let parsed = Doc::parse(&doc(&[1, 2, 3], &index, "")).expect("parses");
        let public = parsed.public().expect("public");
        let expected = expected_shape(&parsed, &public, &["strlen".into()]).expect("expected");
        let items: Vec<&str> = expected
            .iter()
            .filter_map(|r| match r {
                ShapeRecord::Item { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(items, ["c_int", "strlen"]);
        assert_eq!(item_count(&expected), 2);
    }
}
