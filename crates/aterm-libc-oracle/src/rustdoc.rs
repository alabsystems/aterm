// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Crate-root public surface extracted from rustdoc JSON by namespace.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use aterm_json::Value;
/// A document that is not the rustdoc JSON this gate reads (a trustdoc format change).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormatError(pub String);

impl fmt::Display for FormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

pub type Result<T> = std::result::Result<T, FormatError>;

fn missing(what: impl fmt::Display) -> FormatError {
    FormatError(format!("rustdoc JSON lacks {what}"))
}

/// The rustdoc namespace an item kind lives in; other kinds are not public API this gate compares.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Namespace {
    Type,
    Value,
}

impl Namespace {
    #[must_use]
    pub fn of(kind: &str) -> Option<Self> {
        match kind {
            "struct" | "enum" | "union" | "type_alias" | "trait" => Some(Self::Type),
            "function" | "constant" | "static" => Some(Self::Value),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Type => "type",
            Self::Value => "value",
        }
    }
}

/// One crate-root public item.
#[derive(Clone, Copy, Debug)]
pub struct Entry<'d> {
    pub kind: &'d str,
    pub item: &'d Value,
}

impl<'d> Entry<'d> {
    /// The item's `inner.<kind>` object.
    ///
    /// # Errors
    /// When the item has no such object.
    pub fn inner(&self) -> Result<&'d Value> {
        self.item
            .get("inner")
            .and_then(|inner| inner.get(self.kind))
            .ok_or_else(|| missing(format_args!("inner.{} on an item", self.kind)))
    }
}

/// Crate-root public names -> namespace -> the item there.
pub type Public<'d> = BTreeMap<&'d str, BTreeMap<Namespace, Entry<'d>>>;

/// A parsed rustdoc JSON document.
#[derive(Debug)]
pub struct Doc {
    doc: Value,
}

/// The single key of an item's `inner` object (its kind) and that object.
#[must_use]
pub fn kind_of(item: &Value) -> Option<(&str, &Value)> {
    item.get("inner")?
        .as_object()?
        .iter()
        .next()
        .map(|(kind, inner)| (kind.as_str(), inner))
}

/// An item id as the index keys it.
#[must_use]
pub fn id_key(id: &Value) -> Option<String> {
    match id {
        Value::Number(n) => n.as_u64().map(|n| n.to_string()),
        Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

impl Doc {
    /// Parse a rustdoc JSON document.
    ///
    /// # Errors
    /// When the text is not JSON, or has no `index` or `root`.
    pub fn parse(text: &str) -> Result<Self> {
        let doc: Value = aterm_json::from_str(text)
            .map_err(|error| FormatError(format!("rustdoc output is not JSON: {error}")))?;
        if doc.get("index").and_then(Value::as_object).is_none() {
            return Err(missing("an index object"));
        }
        if doc.get("root").is_none() {
            return Err(missing("a root id"));
        }
        Ok(Self { doc })
    }

    /// The item `id` names, if the index has it.
    #[must_use]
    pub fn item(&self, id: &Value) -> Option<&Value> {
        self.doc.get("index")?.get(&id_key(id)?)
    }

    /// The `paths` summary for `id` (crate id and full path), if any.
    #[must_use]
    pub fn path_summary(&self, id: &Value) -> Option<&Value> {
        self.doc.get("paths")?.get(&id_key(id)?)
    }

    /// The crate-root public surface.
    ///
    /// # Errors
    /// When the root module is not in the index.
    pub fn public(&self) -> Result<Public<'_>> {
        let root = self.doc.get("root").ok_or_else(|| missing("a root id"))?;
        if self.item(root).is_none() {
            return Err(missing("the root module in its index"));
        }
        let mut out = Public::new();
        let mut seen = BTreeSet::new();
        self.walk(root, 0, &mut seen, &mut out);
        Ok(out)
    }

    fn walk<'d>(
        &'d self,
        module: &Value,
        depth: usize,
        seen: &mut BTreeSet<String>,
        out: &mut Public<'d>,
    ) {
        const MAX_GLOB_DEPTH: usize = 40;
        let Some(key) = id_key(module) else { return };
        if depth > MAX_GLOB_DEPTH || !seen.insert(key) {
            return;
        }
        let Some(children) = self
            .item(module)
            .and_then(|item| item.get("inner")?.get("module")?.get("items")?.as_array())
        else {
            return;
        };
        for child in children.iter().filter_map(|id| self.item(id)) {
            let Some((kind, inner)) = kind_of(child) else {
                continue;
            };
            match kind {
                "use" => {
                    let Some(target) = inner.get("id").filter(|id| !id.is_null()) else {
                        continue;
                    };
                    if inner.get("is_glob").and_then(Value::as_bool) == Some(true) {
                        self.walk(target, depth + 1, seen, out);
                        continue;
                    }
                    let Some(target_item) = self.item(target) else {
                        continue;
                    };
                    let Some((target_kind, _)) = kind_of(target_item) else {
                        continue;
                    };
                    if target_kind == "module" {
                        continue;
                    }
                    let name = inner
                        .get("name")
                        .and_then(Value::as_str)
                        .or_else(|| target_item.get("name").and_then(Value::as_str));
                    put(out, name, target_kind, target_item);
                }
                "module" => {}
                _ => put(out, child.get("name").and_then(Value::as_str), kind, child),
            }
        }
    }

    /// The impls rustdoc lists on a nominal type, each as its `inner.impl` object.
    pub fn impls<'d>(&'d self, inner: &'d Value) -> impl Iterator<Item = &'d Value> {
        inner
            .get("impls")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|id| self.item(id)?.get("inner")?.get("impl"))
    }

    /// The trait impls written on a type, each with its trait `Path`.
    pub fn declared_trait_impls<'d>(
        &'d self,
        inner: &'d Value,
    ) -> impl Iterator<Item = (&'d Value, &'d Value)> {
        self.impls(inner).filter_map(|imp| {
            let tr = imp.get("trait").filter(|t| !t.is_null())?;
            let synthetic = imp.get("is_synthetic").and_then(Value::as_bool) == Some(true);
            let blanket = imp.get("blanket_impl").is_some_and(|b| !b.is_null());
            (!synthetic && !blanket).then_some((imp, tr))
        })
    }

    /// The public items of a type's inherent impls.
    pub fn public_inherent_items<'d>(
        &'d self,
        inner: &'d Value,
    ) -> impl Iterator<Item = &'d Value> {
        self.impls(inner)
            .filter(|imp| imp.get("trait").is_none_or(Value::is_null))
            .flat_map(|imp| {
                imp.get("items")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
            })
            .filter_map(|id| self.item(id))
            .filter(|item| item.get("visibility").and_then(Value::as_str) == Some("public"))
    }
}

fn put<'d>(out: &mut Public<'d>, name: Option<&'d str>, kind: &'d str, item: &'d Value) {
    let (Some(name), Some(namespace)) = (name.filter(|n| !n.is_empty()), Namespace::of(kind))
    else {
        return;
    };
    out.entry(name)
        .or_default()
        .entry(namespace)
        .or_insert(Entry { kind, item });
}

#[cfg(test)]
pub(crate) mod fixture {
    //! Synthetic rustdoc documents for the unit tests.

    /// Fixture rustdoc JSON document with root module `items`.
    pub fn doc(items: &[u64], index: &str, paths: &str) -> String {
        let items: Vec<String> = items.iter().map(u64::to_string).collect();
        let sep = if index.is_empty() { "" } else { "," };
        format!(
            r#"{{"root":0,"format_version":60,"paths":{{{paths}}},"index":{{
                "0":{{"id":0,"name":"krate","visibility":"public","attrs":[],
                      "inner":{{"module":{{"items":[{}],"is_crate":true}}}}}}{sep}{index}}}}}"#,
            items.join(",")
        )
    }

    /// An index member for a public function.
    pub fn function(id: u64, name: &str, has_body: bool, is_unsafe: bool, attrs: &str) -> String {
        format!(
            r#""{id}":{{"id":{id},"name":"{name}","visibility":"public","attrs":[{attrs}],
                "inner":{{"function":{{"sig":{{"inputs":[],"output":null,"is_c_variadic":false}},
                "header":{{"is_const":false,"is_unsafe":{is_unsafe},"is_async":false,
                           "abi":{{"C":{{"unwind":false}}}}}},
                "has_body":{has_body}}}}}}}"#
        )
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::{doc, function};
    use super::*;

    fn names(public: &Public<'_>) -> Vec<(String, Namespace, String)> {
        public
            .iter()
            .flat_map(|(name, by_ns)| {
                by_ns
                    .iter()
                    .map(|(ns, e)| ((*name).to_string(), *ns, e.kind.to_string()))
            })
            .collect()
    }

    #[test]
    fn a_name_can_be_both_a_type_and_a_function() {
        let text = doc(
            &[1, 2],
            &[
                function(1, "stat", false, true, ""),
                r#""2":{"id":2,"name":"stat","inner":{"struct":{"kind":{"plain":{"fields":[]}},"impls":[]}}}"#
                    .to_string(),
            ]
            .join(","),
            "",
        );
        let parsed = Doc::parse(&text).expect("parses");
        assert_eq!(
            names(&parsed.public().expect("public")),
            [
                ("stat".into(), Namespace::Type, "struct".into()),
                ("stat".into(), Namespace::Value, "function".into()),
            ]
        );
    }

    #[test]
    fn globs_are_walked_renames_resolve_and_modules_are_not_items() {
        let index = [
            r#""1":{"id":1,"inner":{"use":{"source":"self::unix","name":"unix","id":3,"is_glob":true}}}"#,
            r#""2":{"id":2,"inner":{"use":{"source":"x","name":"y","id":5,"is_glob":false}}}"#,
            r#""3":{"id":3,"name":"unix","inner":{"module":{"items":[4,6]}}}"#,
            &function(4, "getpid", false, true, ""),
            r#""5":{"id":5,"name":"x","inner":{"constant":{"type":{"primitive":"i32"},"const":{"expr":"1"}}}}"#,
            r#""6":{"id":6,"name":"inner","inner":{"module":{"items":[7]}}}"#,
            &function(7, "hidden", false, true, ""),
            r#""8":{"id":8,"inner":{"use":{"source":"m","name":"m","id":6,"is_glob":false}}}"#,
        ]
        .join(",");
        let parsed = Doc::parse(&doc(&[1, 2, 6, 8], &index, "")).expect("parses");
        assert_eq!(
            names(&parsed.public().expect("public")),
            [
                ("getpid".into(), Namespace::Value, "function".into()),
                ("y".into(), Namespace::Value, "constant".into()),
            ]
        );
    }

    #[test]
    fn the_first_item_a_name_meets_wins() {
        let index = [
            function(1, "f", false, true, ""),
            function(2, "f", true, false, ""),
        ]
        .join(",");
        let parsed = Doc::parse(&doc(&[1, 2], &index, "")).expect("parses");
        let public = parsed.public().expect("public");
        let entry = public["f"][&Namespace::Value];
        assert_eq!(entry.item.get("id").and_then(Value::as_u64), Some(1));
    }

    #[test]
    fn a_document_without_an_index_or_root_is_a_format_error() {
        assert!(Doc::parse("{}").is_err());
        assert!(Doc::parse(r#"{"index":{}}"#).is_err());
        assert!(Doc::parse("not json").is_err());
        let rootless = Doc::parse(r#"{"index":{},"root":9}"#).expect("parses");
        assert!(rootless.public().is_err());
    }
}
