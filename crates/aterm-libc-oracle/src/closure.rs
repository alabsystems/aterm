// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A cell's DECLARATION SET: its `union.json` names plus every reference-crate name their
//! signatures reach, transitively.

use std::collections::BTreeSet;

use aterm_json::Value;

use crate::rustdoc::{Doc, Entry, Public, Result};

/// The `core::ffi` primitives the shim re-exports on every cell.
pub const PRIMS: [&str; 14] = [
    "c_char",
    "c_schar",
    "c_uchar",
    "c_short",
    "c_ushort",
    "c_int",
    "c_uint",
    "c_long",
    "c_ulong",
    "c_longlong",
    "c_ulonglong",
    "c_float",
    "c_double",
    "c_void",
];

const BUILTIN: [&str; 16] = [
    "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128", "isize", "f32",
    "f64", "bool", "char",
];

const KEYWORDS: [&str; 7] = ["mut", "const", "fn", "extern", "unsafe", "dyn", "Option"];

/// A rustdoc `Type` as unqualified Rust source.
#[must_use]
pub fn render(ty: &Value) -> String {
    const UNMODELLED: &str = "/*?*/()";
    let Some(object) = ty.as_object() else {
        return if ty.is_null() {
            "()".into()
        } else {
            UNMODELLED.into()
        };
    };
    let mut members = object.iter();
    let (Some((kind, inner)), None) = (members.next(), members.next()) else {
        return UNMODELLED.into();
    };
    let field = |key: &str| inner.get(key).unwrap_or(&Value::Null);
    match kind.as_str() {
        "primitive" => match inner.as_str() {
            Some("never") => "!".into(),
            Some(p) => p.into(),
            None => UNMODELLED.into(),
        },
        "resolved_path" => {
            let path = field("path").as_str().unwrap_or_default();
            let base = path.rsplit("::").next().unwrap_or_default();
            let args: Vec<String> = field("args")
                .get("angle_bracketed")
                .and_then(|a| a.get("args"))
                .and_then(Value::as_array)
                .map(|args| {
                    args.iter()
                        .filter_map(|arg| arg.get("type"))
                        .map(render)
                        .collect()
                })
                .unwrap_or_default();
            if args.is_empty() {
                base.into()
            } else {
                format!("{base}<{}>", args.join(", "))
            }
        }
        "raw_pointer" => {
            let mutability = if field("is_mutable").as_bool() == Some(true) {
                "mut"
            } else {
                "const"
            };
            format!("*{mutability} {}", render(field("type")))
        }
        "borrowed_ref" => {
            let mutability = if field("is_mutable").as_bool() == Some(true) {
                "mut "
            } else {
                ""
            };
            format!("&{mutability}{}", render(field("type")))
        }
        "array" => format!(
            "[{}; {}]",
            render(field("type")),
            field("len").as_str().unwrap_or_default()
        ),
        "slice" => format!("[{}]", render(inner)),
        "tuple" => match inner.as_array() {
            Some(items) if !items.is_empty() => {
                let items: Vec<String> = items.iter().map(render).collect();
                format!("({},)", items.join(", "))
            }
            _ => "()".into(),
        },
        "function_pointer" => render_fn_pointer(inner),
        _ => UNMODELLED.into(),
    }
}

fn render_fn_pointer(pointer: &Value) -> String {
    let null = Value::Null;
    let sig = pointer.get("sig").unwrap_or(&null);
    let header = pointer.get("header").unwrap_or(&null);
    let mut args: Vec<String> = sig
        .get("inputs")
        .and_then(Value::as_array)
        .map(|inputs| {
            inputs
                .iter()
                .map(|input| render(input.get_index(1).unwrap_or(&null)))
                .collect()
        })
        .unwrap_or_default();
    if sig.get("is_c_variadic").and_then(Value::as_bool) == Some(true) {
        args.push("...".into());
    }
    let output = match sig.get("output") {
        None | Some(Value::Null) => String::new(),
        Some(ty) => format!(" -> {}", render(ty)),
    };
    let abi = match header.get("abi") {
        Some(Value::String(abi)) if !abi.is_empty() && abi != "Rust" => {
            format!("extern \"{abi}\" ")
        }
        Some(Value::Object(abi)) => abi
            .keys()
            .next()
            .map(|name| format!("extern \"{name}\" "))
            .unwrap_or_default(),
        _ => String::new(),
    };
    let safety = if header.get("is_unsafe").and_then(Value::as_bool) == Some(true) {
        "unsafe "
    } else {
        ""
    };
    format!("{safety}{abi}fn({}){output}", args.join(", "))
}

/// Calls `f` on every identifier in rendered source that could name a crate item.
fn for_each_named(source: &str, mut f: impl FnMut(&str)) {
    let bytes = source.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let starts = bytes[i].is_ascii_alphabetic() || bytes[i] == b'_';
        if !starts {
            i += 1;
            continue;
        }
        let begin = i;
        while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
            i += 1;
        }
        let word = &source[begin..i];
        if !BUILTIN.contains(&word) && !KEYWORDS.contains(&word) && !PRIMS.contains(&word) {
            f(word);
        }
    }
}

/// Every identifier in rendered source that could name a crate item.
#[cfg(test)]
fn named(source: &str, into: &mut BTreeSet<String>) {
    for_each_named(source, |word| {
        into.insert(word.to_string());
    });
}

/// The rendered types one item's signature mentions.
fn signature_types<'d>(doc: &'d Doc, entry: &Entry<'d>) -> Result<Vec<&'d Value>> {
    let inner = entry.inner()?;
    let null = &Value::Null;
    Ok(match entry.kind {
        "constant" | "type_alias" => vec![inner.get("type").unwrap_or(null)],
        "function" => {
            let sig = inner.get("sig").unwrap_or(null);
            let mut types: Vec<&Value> = sig
                .get("inputs")
                .and_then(Value::as_array)
                .map(|inputs| inputs.iter().filter_map(|i| i.get_index(1)).collect())
                .unwrap_or_default();
            if let Some(output) = sig.get("output").filter(|o| !o.is_null()) {
                types.push(output);
            }
            types
        }
        "struct" | "union" => {
            let fields = if entry.kind == "struct" {
                inner
                    .get("kind")
                    .and_then(|k| k.get("plain")?.get("fields"))
            } else {
                inner.get("fields")
            };
            fields
                .and_then(Value::as_array)
                .map(|ids| {
                    ids.iter()
                        .filter_map(|id| doc.item(id)?.get("inner")?.get("struct_field"))
                        .collect()
                })
                .unwrap_or_default()
        }
        _ => Vec::new(),
    })
}

/// `names` plus every name in `public` their signatures reach, transitively.
/// # Errors
/// When a reached item lacks its `inner` object.
pub fn closure(doc: &Doc, public: &Public<'_>, names: &[String]) -> Result<BTreeSet<String>> {
    let mut have: BTreeSet<String> = names.iter().cloned().collect();
    let mut frontier: Vec<String> = have.iter().cloned().collect();
    while !frontier.is_empty() {
        let mut next = Vec::new();
        for name in &frontier {
            let Some(by_namespace) = public.get(name.as_str()) else {
                continue;
            };
            for entry in by_namespace.values() {
                for ty in signature_types(doc, entry)? {
                    for_each_named(&render(ty), |word| {
                        if public.contains_key(word) && !have.contains(word) {
                            let owned = word.to_string();
                            have.insert(owned.clone());
                            next.push(owned);
                        }
                    });
                }
            }
        }
        frontier = next;
    }
    Ok(have)
}

#[cfg(test)]
#[path = "closure_tests.rs"]
mod tests;
