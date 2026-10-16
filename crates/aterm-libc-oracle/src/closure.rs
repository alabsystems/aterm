// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A cell's DECLARATION SET: its `union.json` names plus every reference-crate
//! name their signatures reach, transitively.
//!
//! A named type reachable from a declared item's signature must itself be
//! declared or the shim does not compile, so the shim declares the closure, and
//! the shape gate must expect exactly it. This one function is the set for all
//! three: `regen` declares it in the shim and asserts it in the oracle
//! ([`crate::regen::decls`]), and the shape gate expects it of the shim's
//! rustdoc ([`crate::shape`]). Emitting the oracle from the union list alone
//! once left closure-only types (`cmsghdr`, `vm_statistics64`, …) declared but
//! never checked.
//!
//! Names are read off each signature RENDERED as Rust source, not off the type
//! tree's paths alone: an array length is an expression (`[u8; __PTHREAD_SIZE__]`)
//! and the constant it names is part of the closure.

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

/// A rustdoc `Type` as unqualified Rust source. A shape this does not model
/// renders as `/*?*/()`, which names nothing.
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

/// Every identifier in rendered source that could name a crate item.
fn named(source: &str, into: &mut BTreeSet<String>) {
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
            into.insert(word.to_string());
        }
    }
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
///
/// # Errors
/// When a reached item lacks its `inner` object.
pub fn closure(doc: &Doc, public: &Public<'_>, names: &[String]) -> Result<BTreeSet<String>> {
    let mut have: BTreeSet<String> = names.iter().cloned().collect();
    loop {
        let mut reached = BTreeSet::new();
        for name in &have {
            let Some(by_namespace) = public.get(name.as_str()) else {
                continue;
            };
            for entry in by_namespace.values() {
                for ty in signature_types(doc, entry)? {
                    named(&render(ty), &mut reached);
                }
            }
        }
        let new: Vec<String> = reached
            .into_iter()
            .filter(|name| public.contains_key(name.as_str()) && !have.contains(name))
            .collect();
        if new.is_empty() {
            return Ok(have);
        }
        have.extend(new);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rustdoc::fixture::doc;

    fn ty(json: &str) -> Value {
        aterm_json::from_str(json).expect("json")
    }

    #[test]
    fn types_render_as_unqualified_source() {
        let cases = [
            (r#"{"primitive":"never"}"#, "!"),
            (
                r#"{"resolved_path":{"path":"crate::unix::size_t","id":1,"args":null}}"#,
                "size_t",
            ),
            (
                r#"{"raw_pointer":{"is_mutable":true,"type":{"resolved_path":{"path":"stat","id":2,"args":null}}}}"#,
                "*mut stat",
            ),
            (
                r#"{"array":{"type":{"primitive":"u8"},"len":"__PTHREAD_SIZE__"}}"#,
                "[u8; __PTHREAD_SIZE__]",
            ),
            (r#"{"tuple":[]}"#, "()"),
            (r#"{"tuple":[{"primitive":"i32"}]}"#, "(i32,)"),
            (
                r#"{"function_pointer":{"sig":{"inputs":[["a",{"primitive":"i32"}]],"output":{"primitive":"i32"},"is_c_variadic":true},"header":{"is_unsafe":true,"abi":{"C":{"unwind":false}}}}}"#,
                r#"unsafe extern "C" fn(i32, ...) -> i32"#,
            ),
            (
                r#"{"resolved_path":{"path":"Option","id":3,"args":{"angle_bracketed":{"args":[{"type":{"resolved_path":{"path":"sighandler_t","id":4,"args":null}}}],"constraints":[]}}}}"#,
                "Option<sighandler_t>",
            ),
            (r#"{"generic":"T"}"#, "/*?*/()"),
            ("null", "()"),
        ];
        for (json, rendered) in cases {
            assert_eq!(render(&ty(json)), rendered, "{json}");
        }
    }

    #[test]
    fn identifiers_skip_builtins_keywords_and_primitives() {
        let mut found = BTreeSet::new();
        named(
            r#"unsafe extern "C" fn(*mut stat, c_int, [u8; __PTHREAD_SIZE__]) -> Option<off_t>"#,
            &mut found,
        );
        let found: Vec<&str> = found.iter().map(String::as_str).collect();
        assert_eq!(found, ["C", "__PTHREAD_SIZE__", "off_t", "stat"]);
    }

    /// `f` takes a `*mut s`; `s` has a field of type `t`; `t` is an alias of an
    /// array sized by `N`; `u` is never reached.
    #[test]
    fn the_closure_follows_signatures_fields_aliases_and_array_lengths() {
        let path = |name: &str, id: u64| {
            format!(r#"{{"resolved_path":{{"path":"{name}","id":{id},"args":null}}}}"#)
        };
        let index = [
            format!(
                r#""1":{{"id":1,"name":"f","inner":{{"function":{{"sig":{{"inputs":[["p",{{"raw_pointer":{{"is_mutable":true,"type":{}}}}}]],"output":null}},"header":{{}},"has_body":false}}}}}}"#,
                path("s", 2)
            ),
            r#""2":{"id":2,"name":"s","inner":{"struct":{"kind":{"plain":{"fields":[5]}},"impls":[]}}}"#.into(),
            r#""3":{"id":3,"name":"t","inner":{"type_alias":{"type":{"array":{"type":{"primitive":"u8"},"len":"N"}}}}}"#.into(),
            r#""4":{"id":4,"name":"N","inner":{"constant":{"type":{"primitive":"usize"}}}}"#.into(),
            format!(r#""5":{{"id":5,"name":"x","inner":{{"struct_field":{}}}}}"#, path("t", 3)),
            r#""6":{"id":6,"name":"u","inner":{"constant":{"type":{"primitive":"i32"}}}}"#.into(),
        ]
        .join(",");
        let parsed = Doc::parse(&doc(&[1, 2, 3, 4, 6], &index, "")).expect("parses");
        let public = parsed.public().expect("public");
        let set = closure(&parsed, &public, &["f".into()]).expect("closure");
        let set: Vec<&str> = set.iter().map(String::as_str).collect();
        assert_eq!(set, ["N", "f", "s", "t"]);
    }
}
