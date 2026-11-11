// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Extracts a cell's declaration set ([`crate::closure`]) from the reference `libc` rustdoc JSON.

use std::collections::BTreeSet;

use aterm_json::Value;

use crate::closure::{PRIMS, closure, render};
use crate::rustdoc::{Doc, Public};
use crate::shape::function_has_body;
use crate::symbols::link_name;

/// The explicit upstream trait impls the shim reproduces, in emission order.
pub const DERIVABLE_TRAITS: [&str; 6] = ["Clone", "Copy", "Debug", "PartialEq", "Eq", "Hash"];

/// A type's `#[repr]`, as rustdoc records it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Repr {
    pub align: Option<u64>,
    pub packed: Option<u64>,
    /// The integer of an enum's `#[repr(<int>)]`.
    pub int: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Const {
    pub name: String,
    pub ty: String,
    /// The source expression, when rustdoc kept one (`_` when it did not).
    pub expr: Option<String>,
    /// The evaluated value (`8i32`); `None` for a struct-valued initialiser.
    pub value: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Arg {
    pub name: String,
    pub ty: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Func {
    pub name: String,
    pub is_unsafe: bool,
    pub variadic: bool,
    pub args: Vec<Arg>,
    pub ret: Option<String>,
    pub link_name: Option<String>,
    /// A Rust body (a C macro libc reimplements), not an `extern` declaration.
    pub has_body: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    pub ty: String,
}

/// A struct or a union.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub name: String,
    pub repr: Option<Repr>,
    /// libc keeps private fields (or the struct is not a plain one).
    pub stripped: bool,
    pub traits: Vec<&'static str>,
    pub fields: Vec<Field>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Variant {
    pub name: String,
    pub value: Option<String>,
    pub expr: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Enum {
    pub name: String,
    pub repr: Option<Repr>,
    pub variants: Vec<Variant>,
    pub traits: Vec<&'static str>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alias {
    pub name: String,
    pub ty: String,
}

/// One cell's declarations, each kind in declaration-set (name) order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Decls {
    /// The declaration set: the cell's `union.json` names plus their closure.
    pub set: BTreeSet<String>,
    pub consts: Vec<Const>,
    pub fns: Vec<Func>,
    pub structs: Vec<Record>,
    pub unions: Vec<Record>,
    pub enums: Vec<Enum>,
    pub aliases: Vec<Alias>,
    /// `core::ffi` primitives in the set, which the reference re-exports.
    pub prims: Vec<String>,
    /// Names the reference does not export on this cell: not obligations.
    pub absent: Vec<String>,
    /// Names the closure added to the cell's `union.json` list.
    pub closure_only: Vec<String>,
}

impl Decls {
    /// Every declared name of every kind, the primitives included.
    #[must_use]
    pub fn declared(&self) -> BTreeSet<String> {
        let mut out: BTreeSet<String> = self.prims.iter().cloned().collect();
        out.extend(self.consts.iter().map(|c| c.name.clone()));
        out.extend(self.fns.iter().map(|f| f.name.clone()));
        out.extend(self.structs.iter().map(|r| r.name.clone()));
        out.extend(self.unions.iter().map(|r| r.name.clone()));
        out.extend(self.enums.iter().map(|e| e.name.clone()));
        out.extend(self.aliases.iter().map(|a| a.name.clone()));
        out
    }

    /// Every public field and enum variant, as `(owner, member)`.
    #[must_use]
    pub fn members(&self) -> BTreeSet<(String, String)> {
        let records = self.structs.iter().chain(&self.unions);
        let fields =
            records.flat_map(|r| r.fields.iter().map(|f| (r.name.clone(), f.name.clone())));
        let variants = self
            .enums
            .iter()
            .flat_map(|e| e.variants.iter().map(|v| (e.name.clone(), v.name.clone())));
        fields.chain(variants).collect()
    }
}

fn text(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).map(str::to_string)
}

fn rendered(ty: &Value, what: &str) -> Result<String, String> {
    let out = render(ty);
    if out.contains("/*?*/") {
        return Err(format!("{what}: an unmodelled rustdoc type: {ty}"));
    }
    Ok(out)
}

/// The `#[repr]` among an item's attributes.
#[must_use]
pub fn repr_of(item: &Value) -> Option<Repr> {
    let attrs = item.get("attrs")?.as_array()?;
    let repr = attrs.iter().find_map(|attr| attr.get("repr"))?;
    Some(Repr {
        align: repr.get("align").and_then(Value::as_u64),
        packed: repr.get("packed").and_then(Value::as_u64),
        int: text(repr.get("int")),
    })
}

/// Returns explicit derivable trait impls on `inner` in [`DERIVABLE_TRAITS`] order.
///
/// # Errors
/// On a negative or generic impl the shim cannot reproduce with `#[derive]`.
pub fn derivable_traits(name: &str, inner: &Value, doc: &Doc) -> Result<Vec<&'static str>, String> {
    let mut found = BTreeSet::new();
    for (imp, tr) in doc.declared_trait_impls(inner) {
        let Some(path) = tr.get("path").and_then(Value::as_str) else {
            continue;
        };
        let seg = path.rsplit("::").next().unwrap_or(path);
        let Some(trait_name) = DERIVABLE_TRAITS.iter().find(|t| **t == seg) else {
            continue;
        };
        let negative = imp.get("is_negative").and_then(Value::as_bool) == Some(true);
        if negative || tr.get("args").is_some_and(|a| !a.is_null()) {
            return Err(format!(
                "cannot derive non-plain trait impl {path} for {name}"
            ));
        }
        found.insert(*trait_name);
    }
    Ok(DERIVABLE_TRAITS
        .into_iter()
        .filter(|t| found.contains(t))
        .collect())
}

fn member<'d>(doc: &'d Doc, id: &Value, owner: &str) -> Result<(String, &'d Value), String> {
    let item = doc
        .item(id)
        .ok_or_else(|| format!("{owner}: a member id the index lacks"))?;
    let name = text(item.get("name")).ok_or_else(|| format!("{owner}: an unnamed member"))?;
    Ok((name, item))
}

fn fields(doc: &Doc, ids: &[Value], owner: &str) -> Result<Vec<Field>, String> {
    ids.iter()
        .map(|id| {
            let (name, item) = member(doc, id, owner)?;
            let ty = item
                .get("inner")
                .and_then(|i| i.get("struct_field"))
                .ok_or_else(|| format!("{owner}.{name}: not a struct field"))?;
            let ty = rendered(ty, &format!("{owner}.{name}"))?;
            Ok(Field { name, ty })
        })
        .collect()
}

fn record(
    name: &str,
    kind: &str,
    item: &Value,
    inner: &Value,
    doc: &Doc,
) -> Result<Record, String> {
    let no_fields = Vec::new();
    let (ids, stripped) = if kind == "struct" {
        match inner.get("kind").and_then(|k| k.get("plain")) {
            Some(plain) => (
                plain
                    .get("fields")
                    .and_then(Value::as_array)
                    .unwrap_or(&no_fields),
                plain.get("has_stripped_fields").and_then(Value::as_bool) == Some(true),
            ),
            None => (&no_fields, true),
        }
    } else {
        (
            inner
                .get("fields")
                .and_then(Value::as_array)
                .unwrap_or(&no_fields),
            inner.get("has_stripped_fields").and_then(Value::as_bool) == Some(true),
        )
    };
    Ok(Record {
        name: name.into(),
        repr: repr_of(item),
        stripped,
        traits: derivable_traits(name, inner, doc)?,
        fields: fields(doc, ids, name)?,
    })
}

fn function(name: &str, item: &Value, inner: &Value) -> Result<Func, String> {
    let sig = inner
        .get("sig")
        .ok_or_else(|| format!("{name}: no signature"))?;
    let header = inner.get("header").unwrap_or(&Value::Null);
    let inputs = sig
        .get("inputs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten();
    let args = inputs
        .map(|input| {
            let arg = text(input.get_index(0)).unwrap_or_default();
            let ty = input.get_index(1).unwrap_or(&Value::Null);
            Ok(Arg {
                ty: rendered(ty, &format!("{name}({arg})"))?,
                name: arg,
            })
        })
        .collect::<Result<_, String>>()?;
    let ret = match sig.get("output") {
        None | Some(Value::Null) => None,
        Some(ty) => Some(rendered(ty, &format!("{name} -> "))?),
    };
    Ok(Func {
        name: name.into(),
        is_unsafe: header.get("is_unsafe").and_then(Value::as_bool) == Some(true),
        variadic: sig.get("is_c_variadic").and_then(Value::as_bool) == Some(true),
        args,
        ret,
        link_name: link_name(item),
        has_body: function_has_body(name, item).map_err(|e| e.0)?,
    })
}

fn variants(doc: &Doc, inner: &Value, owner: &str) -> Result<Vec<Variant>, String> {
    let ids = inner
        .get("variants")
        .and_then(Value::as_array)
        .into_iter()
        .flatten();
    ids.map(|id| {
        let (name, item) = member(doc, id, owner)?;
        let disc = item
            .get("inner")
            .and_then(|i| i.get("variant")?.get("discriminant"))
            .filter(|d| !d.is_null());
        Ok(Variant {
            name,
            value: text(disc.and_then(|d| d.get("value"))),
            expr: text(disc.and_then(|d| d.get("expr"))),
        })
    })
    .collect()
}

/// Reads the cell's `union.json` declarations and their closure from `doc`.
///
/// # Errors
/// When a reached item cannot be declared by the shim or lacks required rustdoc metadata.
pub fn read(doc: &Doc, public: &Public<'_>, union: &[String]) -> Result<Decls, String> {
    let names = closure(doc, public, union).map_err(|e| e.0)?;
    let listed: BTreeSet<&str> = union.iter().map(String::as_str).collect();
    let mut out = Decls {
        closure_only: names
            .iter()
            .filter(|n| !listed.contains(n.as_str()))
            .cloned()
            .collect(),
        set: names.clone(),
        ..Decls::default()
    };
    for name in &names {
        let Some(by_namespace) = public.get(name.as_str()) else {
            if PRIMS.contains(&name.as_str()) {
                out.prims.push(name.clone());
            } else {
                out.absent.push(name.clone());
            }
            continue;
        };
        for entry in by_namespace.values() {
            let inner = entry.inner().map_err(|e| e.0)?;
            let ty = || inner.get("type").unwrap_or(&Value::Null);
            match entry.kind {
                "constant" => {
                    let value = inner.get("const");
                    out.consts.push(Const {
                        name: name.clone(),
                        ty: rendered(ty(), name)?,
                        expr: text(value.and_then(|c| c.get("expr"))),
                        value: text(value.and_then(|c| c.get("value"))),
                    });
                }
                "function" => out.fns.push(function(name, entry.item, inner)?),
                "struct" => out
                    .structs
                    .push(record(name, "struct", entry.item, inner, doc)?),
                "union" => out
                    .unions
                    .push(record(name, "union", entry.item, inner, doc)?),
                "enum" => {
                    if inner.get("has_stripped_variants").and_then(Value::as_bool) == Some(true) {
                        return Err(format!("{name}: an enum with private variants"));
                    }
                    out.enums.push(Enum {
                        name: name.clone(),
                        repr: repr_of(entry.item),
                        variants: variants(doc, inner, name)?,
                        traits: derivable_traits(name, inner, doc)?,
                    });
                }
                "type_alias" => out.aliases.push(Alias {
                    name: name.clone(),
                    ty: rendered(ty(), name)?,
                }),
                other => return Err(format!("{name}: the shim cannot declare a {other}")),
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
#[path = "decls_tests.rs"]
mod tests;
