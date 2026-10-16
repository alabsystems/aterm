// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The conformance oracle's cell module, `libc-oracle/conformance/src/
//! cell_<cell>.rs`, written from the reference's rustdoc over the cell's
//! declaration set, and its COVERAGE AUDIT.
//!
//! Every assertion compares the shim (`libc_shim`) with the reference
//! (`libc_real`): each constant's type and value, each function's signature,
//! each type's size and alignment, each public field's offset and type, each
//! discriminant, each alias's identity, each `siginfo_t` accessor's signature,
//! and the behaviour of every hand-written body ([`super::behaviour`]).
//!
//! The ledger records what was actually asserted, name by name and field by
//! field, and the audit diffs it against what the shim DECLARES ([`Decls`], the
//! emitter's own input) — never against the list the assertions came from, which
//! would share their blind spot and read zero uncovered forever.

use std::collections::{BTreeMap, BTreeSet};

use aterm_json::Value;

use super::REFERENCE_VERSION;
use super::behaviour;
use super::decls::Decls;
use super::handwritten;
use crate::cells::Cell;
use crate::closure::PRIMS;
use crate::rustdoc::{Doc, Entry, Namespace, Public};

/// The shim under test, as the conformance crate names it.
pub const SHIM: &str = "libc_shim";
/// The reference libc, as the conformance crate names it.
pub const REAL: &str = "libc_real";

const INT_PRIMS: [&str; 12] = [
    "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128", "isize",
];

/// How deep a type is followed through aliases and pointers.
const MAX_DEPTH: usize = 12;

/// The single member of a rustdoc `Type` object: its kind and payload.
fn single(ty: &Value) -> Option<(&str, &Value)> {
    let object = ty.as_object()?;
    let mut members = object.iter();
    match (members.next(), members.next()) {
        (Some((kind, inner)), None) => Some((kind.as_str(), inner)),
        _ => None,
    }
}

fn last_segment(path: &Value) -> &str {
    let path = path.get("path").and_then(Value::as_str).unwrap_or_default();
    path.rsplit("::").next().unwrap_or_default()
}

fn generic_types(path: &Value) -> impl Iterator<Item = &Value> {
    path.get("args")
        .and_then(|a| a.get("angle_bracketed")?.get("args")?.as_array())
        .into_iter()
        .flatten()
        .filter_map(|arg| arg.get("type"))
}

fn is_true(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool) == Some(true)
}

/// A function header's ABI as written before `fn`.
fn abi(header: &Value) -> Result<String, String> {
    match header.get("abi") {
        None | Some(Value::Null) => Ok(String::new()),
        Some(Value::String(abi)) if abi == "Rust" => Ok(String::new()),
        Some(Value::String(abi)) => Ok(format!("extern \"{abi}\" ")),
        Some(Value::Object(abi)) if abi.len() == 1 => Ok(format!(
            "extern \"{}\" ",
            abi.keys().next().map_or("", String::as_str)
        )),
        Some(other) => Err(format!("an unmodelled ABI {other}")),
    }
}

/// The reference's rustdoc, read the way the assertions need it.
struct Reference<'d> {
    doc: &'d Doc,
    public: &'d Public<'d>,
}

impl<'d> Reference<'d> {
    fn entry(&self, name: &str, namespace: Namespace) -> Option<Entry<'d>> {
        self.public.get(name)?.get(&namespace).copied()
    }

    fn alias_target(&self, name: &str) -> Option<&'d Value> {
        let entry = self.entry(name, Namespace::Type)?;
        if entry.kind != "type_alias" {
            return None;
        }
        entry.inner().ok()?.get("type")
    }

    /// A rustdoc `Type` as source, each named type qualified with `krate`.
    fn render(&self, ty: &Value, krate: &str) -> Result<String, String> {
        if ty.is_null() {
            return Ok("()".into());
        }
        let (kind, inner) = single(ty).ok_or_else(|| format!("not a rustdoc type: {ty}"))?;
        let field = |key: &str| inner.get(key).unwrap_or(&Value::Null);
        Ok(match kind {
            "primitive" => match inner.as_str() {
                Some("never") => "!".into(),
                Some(primitive) => primitive.into(),
                None => return Err(format!("a primitive without a name: {ty}")),
            },
            "resolved_path" => {
                let segment = last_segment(inner);
                let args = generic_types(inner)
                    .map(|arg| self.render(arg, krate))
                    .collect::<Result<Vec<_>, _>>()?;
                let args = if args.is_empty() {
                    String::new()
                } else {
                    format!("<{}>", args.join(", "))
                };
                if self.public.contains_key(segment) || PRIMS.contains(&segment) {
                    format!("{krate}::{segment}{args}")
                } else if segment == "Option" {
                    // core's own `Option`, ONE type in both crates: written
                    // unqualified, its argument carries the contract.
                    format!("Option{args}")
                } else {
                    return Err(format!("an unresolved named type {segment}"));
                }
            }
            "raw_pointer" => format!(
                "*{} {}",
                if is_true(inner, "is_mutable") {
                    "mut"
                } else {
                    "const"
                },
                self.render(field("type"), krate)?
            ),
            "borrowed_ref" => format!(
                "&{}{}",
                if is_true(inner, "is_mutable") {
                    "mut "
                } else {
                    ""
                },
                self.render(field("type"), krate)?
            ),
            "array" => format!(
                "[{}; {}]",
                self.render(field("type"), krate)?,
                field("len").as_str().unwrap_or_default()
            ),
            "slice" => format!("[{}]", self.render(inner, krate)?),
            "tuple" => match inner.as_array() {
                Some(items) if !items.is_empty() => {
                    let items = items
                        .iter()
                        .map(|item| self.render(item, krate))
                        .collect::<Result<Vec<_>, _>>()?;
                    format!("({},)", items.join(", "))
                }
                _ => "()".into(),
            },
            "function_pointer" => {
                let sig = field("sig");
                let inputs = sig
                    .get("inputs")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten();
                let args = inputs.map(|input| input.get_index(1).unwrap_or(&Value::Null));
                self.fn_type(
                    sig,
                    field("header"),
                    args.map(|a| self.render(a, krate)),
                    krate,
                )?
            }
            other => return Err(format!("an unmodelled type kind {other}")),
        })
    }

    /// `unsafe extern "C" fn(args) -> out`, from a signature and its header.
    fn fn_type(
        &self,
        sig: &Value,
        header: &Value,
        args: impl Iterator<Item = Result<String, String>>,
        krate: &str,
    ) -> Result<String, String> {
        let mut args = args.collect::<Result<Vec<_>, _>>()?;
        if is_true(sig, "is_c_variadic") {
            args.push("...".into());
        }
        let output = match sig.get("output") {
            None | Some(Value::Null) => String::new(),
            Some(output) => format!(" -> {}", self.render(output, krate)?),
        };
        Ok(format!(
            "{}{}fn({}){output}",
            if is_true(header, "is_unsafe") {
                "unsafe "
            } else {
                ""
            },
            abi(header)?,
            args.join(", ")
        ))
    }

    /// Whether `ty` names no nominal (struct, enum, union) type: it is then ONE
    /// type in both crates, and a pin can share it.
    fn transparent(&self, ty: &Value, depth: usize) -> bool {
        if ty.is_null() || depth > MAX_DEPTH {
            return true;
        }
        let Some((kind, inner)) = single(ty) else {
            return false;
        };
        let deeper = |t: &Value| self.transparent(t, depth + 1);
        let field = |key: &str| inner.get(key).unwrap_or(&Value::Null);
        match kind {
            "primitive" => true,
            "resolved_path" => {
                let segment = last_segment(inner);
                if PRIMS.contains(&segment) {
                    true
                } else if segment == "Option" {
                    generic_types(inner).all(deeper)
                } else {
                    self.alias_target(segment).is_some_and(deeper)
                }
            }
            "raw_pointer" | "borrowed_ref" | "array" => deeper(field("type")),
            "slice" => deeper(inner),
            "tuple" => inner.as_array().into_iter().flatten().all(deeper),
            "function_pointer" => {
                let sig = field("sig");
                let mut inputs = sig
                    .get("inputs")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten();
                inputs.all(|input| deeper(input.get_index(1).unwrap_or(&Value::Null)))
                    && deeper(sig.get("output").unwrap_or(&Value::Null))
            }
            _ => false,
        }
    }

    /// Whether `ty` bottoms out in an integer primitive (castable `as i128`).
    fn int_typed(&self, ty: &Value, depth: usize) -> bool {
        if ty.is_null() || depth > MAX_DEPTH {
            return false;
        }
        match single(ty) {
            Some(("primitive", inner)) => inner.as_str().is_some_and(|p| INT_PRIMS.contains(&p)),
            Some(("resolved_path", inner)) => {
                let segment = last_segment(inner);
                if PRIMS.contains(&segment) {
                    !matches!(segment, "c_void" | "c_float" | "c_double")
                } else {
                    self.alias_target(segment)
                        .is_some_and(|t| self.int_typed(t, depth + 1))
                }
            }
            _ => false,
        }
    }

    /// The exact public inherent method set of one nominal type.
    fn inherent_methods(&self, type_name: &str) -> Result<BTreeMap<String, &'d Value>, String> {
        let mut out = BTreeMap::new();
        let Some(entry) = self.entry(type_name, Namespace::Type) else {
            return Ok(out);
        };
        let inner = entry.inner().map_err(|e| e.0)?;
        for method in self.doc.public_inherent_items(inner) {
            let function = method.get("inner").and_then(|i| i.get("function"));
            let name = method
                .get("name")
                .and_then(Value::as_str)
                .filter(|n| !n.is_empty());
            let (Some(function), Some(name)) = (function, name) else {
                continue;
            };
            if out.insert(name.to_string(), function).is_some() {
                return Err(format!(
                    "{type_name} has duplicate public inherent method {name}"
                ));
            }
        }
        Ok(out)
    }

    /// An inherent method as an ordinary function-pointer type in `krate`.
    fn method_type(
        &self,
        type_name: &str,
        function: &Value,
        krate: &str,
    ) -> Result<String, String> {
        let sig = function.get("sig").unwrap_or(&Value::Null);
        let inputs = sig
            .get("inputs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten();
        let args = inputs.map(|input| {
            let ty = input.get_index(1).unwrap_or(&Value::Null);
            if input.get_index(0).and_then(Value::as_str) != Some("self") {
                return self.render(ty, krate);
            }
            let borrowed = ty
                .get("borrowed_ref")
                .filter(|b| b.get("type").and_then(|t| t.get("generic")?.as_str()) == Some("Self"))
                .ok_or_else(|| {
                    format!("unsupported receiver for {type_name} inherent method: {ty}")
                })?;
            let mutability = if is_true(borrowed, "is_mutable") {
                "mut "
            } else {
                ""
            };
            Ok(format!("&{mutability}{krate}::{type_name}"))
        });
        self.fn_type(
            sig,
            function.get("header").unwrap_or(&Value::Null),
            args,
            krate,
        )
    }
}

/// What the oracle asserted, recorded as it was emitted.
#[derive(Debug, Default)]
pub struct Ledger {
    /// Name -> the kinds of check asserted about it.
    pub checked: BTreeMap<String, BTreeSet<&'static str>>,
    /// `(type, field or variant)` pairs asserted.
    pub members: BTreeSet<(String, String)>,
    /// `(type, method)` pairs whose signature was pinned.
    pub methods: BTreeSet<(String, String)>,
    pub stats: BTreeMap<&'static str, usize>,
}

impl Ledger {
    fn count(&mut self, stat: &'static str) {
        *self.stats.entry(stat).or_default() += 1;
    }

    fn check(&mut self, name: &str, kind: &'static str) {
        self.checked.entry(name.into()).or_default().insert(kind);
    }
}

/// Emits one cell's assertions.
struct Asserter<'r, 'd> {
    reference: &'r Reference<'d>,
    triple: &'static str,
    lines: Vec<String>,
    /// `(name, body)` of the checks only a native run can execute.
    runtime: Vec<(String, String)>,
    ledger: Ledger,
}

impl Asserter<'_, '_> {
    fn line(&mut self, line: String) {
        self.lines.push(line);
    }

    fn size_align(&mut self, name: &str) {
        let triple = self.triple;
        for measure in ["size_of", "align_of"] {
            self.line(format!(
                "const _: () = assert!(::core::mem::{measure}::<{SHIM}::{name}>() == ::core::mem::{measure}::<{REAL}::{name}>(), \"libc {name}: {measure} differs on {triple}\");"
            ));
        }
    }

    fn identity(&mut self, name: &str) {
        self.line(format!(
            "const _: fn({SHIM}::{name}) -> {REAL}::{name} = |x| x;"
        ));
        self.line(format!(
            "const _: fn({REAL}::{name}) -> {SHIM}::{name} = |x| x;"
        ));
    }

    fn missing(&mut self, name: &str) {
        if name == "c_void" {
            // An opaque enum, not a value type: its layout is its contract.
            self.size_align("c_void");
            self.ledger.count("prim");
            self.ledger.check(name, "prim_layout");
        } else if PRIMS.contains(&name) {
            // Identity coercion BOTH ways: the same type, not two of one width.
            self.identity(name);
            self.ledger.count("prim");
            self.ledger.check(name, "prim_identity");
        } else {
            self.line(format!(
                "// !! {name}: ABSENT from oracle libc on this cell"
            ));
            self.ledger.count("absent");
        }
    }

    fn constant(&mut self, name: &str, inner: &Value) -> Result<(), String> {
        let (r, triple) = (self.reference, self.triple);
        let ty = inner.get("type").unwrap_or(&Value::Null);
        let (shim_ty, real_ty) = (r.render(ty, SHIM)?, r.render(ty, REAL)?);
        if r.transparent(ty, 0) {
            self.line(format!("const _: {real_ty} = {SHIM}::{name};"));
        } else {
            self.line(format!("const _: {shim_ty} = {SHIM}::{name};"));
            self.line(format!("const _: {real_ty} = {REAL}::{name};"));
        }
        if r.int_typed(ty, 0) {
            self.line(format!(
                "const _: () = assert!({SHIM}::{name} as i128 == {REAL}::{name} as i128, \"libc constant {name} differs on {triple}\");"
            ));
            self.ledger.count("const_value_ct");
        } else if single(ty).is_some_and(|(kind, _)| kind == "raw_pointer") {
            self.runtime.push((
                name.into(),
                format!(
                    "assert_eq!({SHIM}::{name} as usize, {REAL}::{name} as usize, \"libc pointer constant {name} differs\");"
                ),
            ));
            self.ledger.count("const_value_rt");
        } else {
            // A struct-valued initialiser: compared byte for byte.
            self.lines.extend([
                "const _: () = {".to_string(),
                format!("    const NS: usize = ::core::mem::size_of::<{shim_ty}>();"),
                format!("    const NR: usize = ::core::mem::size_of::<{real_ty}>();"),
                format!("    assert!(NS == NR, \"libc constant {name}: type size differs\");"),
                format!("    const A: [u8; NS] = unsafe {{ ::core::mem::transmute({SHIM}::{name}) }};"),
                format!("    const B: [u8; NR] = unsafe {{ ::core::mem::transmute({REAL}::{name}) }};"),
                format!(
                    "    assert!(crate::bytes_eq(&A, &B), \"libc constant {name} differs on {triple}\");"
                ),
                "};".to_string(),
            ]);
            self.ledger.count("const_value_bytes");
        }
        self.ledger.count("constant");
        self.ledger.check(name, "constant");
        Ok(())
    }

    fn function(&mut self, name: &str, inner: &Value) -> Result<(), String> {
        let r = self.reference;
        let sig = inner.get("sig").unwrap_or(&Value::Null);
        let header = inner.get("header").unwrap_or(&Value::Null);
        let inputs: Vec<&Value> = sig
            .get("inputs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|input| input.get_index(1).unwrap_or(&Value::Null))
            .collect();
        let fn_type = |krate| {
            r.fn_type(
                sig,
                header,
                inputs.iter().map(|ty| r.render(ty, krate)),
                krate,
            )
        };
        let shared = inputs.iter().all(|ty| r.transparent(ty, 0))
            && r.transparent(sig.get("output").unwrap_or(&Value::Null), 0);
        let real_ty = fn_type(REAL)?;
        if shared {
            // The strongest form: both items coerce to ONE fn-pointer type.
            self.line(format!("const _: {real_ty} = {SHIM}::{name};"));
            self.line(format!("const _: {real_ty} = {REAL}::{name};"));
            self.ledger.count("fn_shared");
        } else {
            // Mirrored: one shape, each in its own crate's nominal types.
            self.line(format!("const _: {} = {SHIM}::{name};", fn_type(SHIM)?));
            self.line(format!("const _: {real_ty} = {REAL}::{name};"));
            self.ledger.count("fn_mirrored");
        }
        self.ledger.count("function");
        self.ledger.check(name, "function");
        Ok(())
    }

    fn record(&mut self, name: &str, kind: &str, inner: &Value) -> Result<(), String> {
        let (r, triple) = (self.reference, self.triple);
        self.size_align(name);
        self.ledger.count("type_sizealign");
        self.ledger.check(name, "size_align");
        let plain = (kind == "struct")
            .then(|| inner.get("kind").and_then(|k| k.get("plain")))
            .flatten();
        let (ids, stripped) = match (kind, plain) {
            ("struct", Some(plain)) => (plain.get("fields"), is_true(plain, "has_stripped_fields")),
            ("struct", None) => (None, false),
            _ => (inner.get("fields"), is_true(inner, "has_stripped_fields")),
        };
        let fields: Vec<&Value> = ids
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|id| r.doc.item(id))
            .collect();
        for field in &fields {
            let f = field
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            self.line(format!(
                "const _: () = assert!(::core::mem::offset_of!({SHIM}::{name}, {f}) == ::core::mem::offset_of!({REAL}::{name}, {f}), \"libc {name}.{f}: field offset differs on {triple}\");"
            ));
            self.ledger.count("field_offset");
            self.ledger.members.insert((name.into(), f.into()));
            let ty = field
                .get("inner")
                .and_then(|i| i.get("struct_field"))
                .ok_or_else(|| format!("{name}.{f}: not a struct field"))?;
            let read = if kind == "union" {
                format!("|v| unsafe {{ v.{f} }}")
            } else {
                format!("|v| v.{f}")
            };
            if r.transparent(ty, 0) {
                // The field has ONE type in both crates.
                let real_ty = r.render(ty, REAL)?;
                self.line(format!(
                    "const _: fn({SHIM}::{name}) -> {real_ty} = {read};"
                ));
                self.ledger.count("field_type");
            } else {
                // A named type of this cell, pinned on each side to its own
                // crate's item of that name, whose own shape is asserted in turn.
                let (shim_ty, real_ty) = (r.render(ty, SHIM)?, r.render(ty, REAL)?);
                self.line(format!(
                    "const _: fn({SHIM}::{name}) -> {shim_ty} = {read};"
                ));
                self.line(format!(
                    "const _: fn({REAL}::{name}) -> {real_ty} = {read};"
                ));
                self.ledger.count("field_type_mirrored");
            }
        }
        if stripped {
            self.line(format!(
                "// {name}: has private fields; size_of/align_of are the only public contract for them"
            ));
            self.ledger.count("struct_stripped");
        }
        if fields.is_empty() {
            self.ledger.count("struct_opaque");
        }
        if name == "siginfo_t" {
            // The exact public inherent method set, each signature pinned; the
            // receiver makes every one nominal.
            for (method, function) in r.inherent_methods(name)? {
                let shim_ty = r.method_type(name, function, SHIM)?;
                let real_ty = r.method_type(name, function, REAL)?;
                self.line(format!("const _: {shim_ty} = {SHIM}::{name}::{method};"));
                self.line(format!("const _: {real_ty} = {REAL}::{name}::{method};"));
                self.ledger.count("inherent_method");
                self.ledger.methods.insert((name.into(), method));
            }
        }
        self.ledger.count("type");
        Ok(())
    }

    fn enumeration(&mut self, name: &str, inner: &Value) {
        let (r, triple) = (self.reference, self.triple);
        self.size_align(name);
        self.ledger.count("type_sizealign");
        self.ledger.check(name, "size_align");
        let variants = inner
            .get("variants")
            .and_then(Value::as_array)
            .into_iter()
            .flatten();
        for variant in variants.filter_map(|id| r.doc.item(id)) {
            let v = variant
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            self.line(format!(
                "const _: () = assert!({SHIM}::{name}::{v} as i128 == {REAL}::{name}::{v} as i128, \"libc {name}::{v}: discriminant differs on {triple}\");"
            ));
            self.ledger.count("enum_variant");
            self.ledger.members.insert((name.into(), v.into()));
        }
        self.ledger.count("type");
    }

    fn alias(&mut self, name: &str, inner: &Value) -> Result<(), String> {
        let r = self.reference;
        let ty = inner.get("type").unwrap_or(&Value::Null);
        if r.transparent(ty, 0) {
            // Identity coercion: the two aliases are the SAME type.
            self.identity(name);
            self.ledger.count("alias_identity");
            self.ledger.check(name, "alias_identity");
        } else {
            // A nominal referent is a distinct type in each crate, so pin each
            // alias both ways to the reference type rendered into its own crate:
            // size and alignment alone let a pointer drift to any of one width.
            for krate in [SHIM, REAL] {
                let target = r.render(ty, krate)?;
                self.line(format!("const _: fn({krate}::{name}) -> {target} = |x| x;"));
                self.line(format!("const _: fn({target}) -> {krate}::{name} = |x| x;"));
            }
            self.size_align(name);
            self.ledger.count("alias_nominal_identity");
            self.ledger.check(name, "alias_nominal_identity");
        }
        self.ledger.count("type_alias");
        Ok(())
    }

    fn item(&mut self, name: &str) -> Result<(), String> {
        let Some(by_namespace) = self.reference.public.get(name) else {
            self.missing(name);
            return Ok(());
        };
        for entry in by_namespace.values() {
            let inner = entry.inner().map_err(|e| e.0)?;
            match entry.kind {
                "constant" => self.constant(name, inner)?,
                "function" => self.function(name, inner)?,
                "struct" | "union" => self.record(name, entry.kind, inner)?,
                "enum" => self.enumeration(name, inner),
                "type_alias" => self.alias(name, inner)?,
                other => return Err(format!("{name}: the oracle cannot check a {other}")),
            }
        }
        Ok(())
    }
}

/// What one cell's oracle checks, and what its audit found.
#[derive(Debug)]
pub struct Generated {
    /// The module's text, before formatting.
    pub text: String,
    pub stats: BTreeMap<&'static str, usize>,
    pub checked_items: usize,
    pub checked_members: usize,
    pub runtime_tests: usize,
    /// Coverage-audit failures: each a declaration or body the oracle does not
    /// check, or a check of something the shim does not declare.
    pub audit: Vec<String>,
}

/// The `const fn` flag of a reference function, by its rustdoc header.
fn is_const_fn(entry: &Entry<'_>) -> Result<bool, String> {
    let header = entry
        .inner()
        .map_err(|e| e.0)?
        .get("header")
        .ok_or_else(|| "a function without a header".to_string())?;
    Ok(is_true(header, "is_const"))
}

fn named<T: std::fmt::Debug>(what: &str, items: &BTreeSet<T>) -> Option<String> {
    (!items.is_empty()).then(|| format!("{what}: {items:?}"))
}

/// The audit of the hand-written bodies: every macro the reference makes a
/// Rust body has a behaviour sweep and every sweep a macro; every reference
/// accessor is emitted and swept, and nothing else is.
fn handwritten_gaps(
    expected_macros: &BTreeSet<String>,
    checked_macros: &BTreeSet<String>,
    reference_methods: &BTreeSet<String>,
    emitted_methods: &BTreeSet<String>,
    checked_methods: &BTreeSet<String>,
) -> Vec<String> {
    let gap = |a: &BTreeSet<String>, b: &BTreeSet<String>| -> BTreeSet<String> {
        a.difference(b).cloned().collect()
    };
    [
        named(
            "MACROS WITHOUT BEHAVIOR",
            &gap(expected_macros, checked_macros),
        ),
        named(
            "BEHAVIOR WITHOUT MACRO",
            &gap(checked_macros, expected_macros),
        ),
        named(
            "REFERENCE METHODS NOT EMITTED",
            &gap(reference_methods, emitted_methods),
        ),
        named(
            "EMITTED METHODS NOT IN REFERENCE",
            &gap(emitted_methods, reference_methods),
        ),
        named(
            "METHODS WITHOUT BEHAVIOR",
            &gap(reference_methods, checked_methods),
        ),
        named(
            "BEHAVIOR WITHOUT METHOD",
            &gap(checked_methods, reference_methods),
        ),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// The coverage audit against what the shim declares.
fn coverage(decls: &Decls, ledger: &Ledger) -> Vec<String> {
    let set = &decls.set;
    let checked: BTreeSet<String> = ledger
        .checked
        .iter()
        .filter(|(_, kinds)| !kinds.is_empty())
        .map(|(name, _)| name.clone())
        .collect();
    let declared = decls.declared();
    let absent: BTreeSet<String> = decls.absent.iter().cloned().collect();
    let prims: BTreeSet<String> = PRIMS.iter().map(|p| (*p).to_string()).collect();
    let present: BTreeSet<String> = set.difference(&absent).cloned().collect();
    let drift: BTreeSet<String> = declared
        .difference(set)
        .chain(
            present
                .difference(&declared)
                .filter(|n| !prims.contains(*n)),
        )
        .cloned()
        .collect();
    let uncovered: BTreeSet<String> = declared.difference(&checked).cloned().collect();
    let members: BTreeSet<String> = decls
        .members()
        .difference(&ledger.members)
        .map(|(owner, member)| format!("{owner}.{member}"))
        .collect();
    [
        named(
            "THE SHIM DECLARES A DIFFERENT SET THAN THE ORACLE CHECKS",
            &drift,
        ),
        named("DECLARED BUT NOT CHECKED", &uncovered),
        named("FIELDS DECLARED BUT NOT CHECKED", &members),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// The cell's oracle module and its audit.
///
/// # Errors
/// When the reference holds an item or type the oracle cannot check.
pub fn emit(
    cell: &Cell,
    doc: &Doc,
    public: &Public<'_>,
    decls: &Decls,
) -> Result<Generated, String> {
    let reference = Reference { doc, public };
    let mut asserter = Asserter {
        reference: &reference,
        triple: cell.triple,
        lines: Vec::new(),
        runtime: Vec::new(),
        ledger: Ledger::default(),
    };
    for name in &decls.set {
        asserter.item(name)?;
    }
    let declares =
        |name: &str| decls.set.contains(name) && reference.entry(name, Namespace::Value).is_some();
    let mut evaluated = Vec::new();
    let mut native: Vec<(String, String)> = Vec::new();
    let mut checked_macros = BTreeSet::new();
    for (name, sweep) in behaviour::evaluable() {
        let Some(entry) = reference
            .entry(name, Namespace::Value)
            .filter(|_| declares(name))
        else {
            continue;
        };
        let sweep = behaviour::crates(&sweep);
        if is_const_fn(&entry)? {
            evaluated.push(format!(
                "// behaviour sweep: {name}\nconst _: () = {{ {sweep} }};"
            ));
            asserter.ledger.count("behaviour_ct");
        } else {
            native.push((name.into(), sweep));
            asserter.ledger.count("behaviour_rt");
        }
        checked_macros.insert(name.to_string());
    }
    for (name, sweep) in behaviour::NATIVE {
        if declares(name) {
            native.push((name.into(), behaviour::crates(sweep)));
            asserter.ledger.count("behaviour_rt");
            checked_macros.insert(name.to_string());
        }
    }
    let reference_methods = reference.inherent_methods("siginfo_t")?;
    let mut checked_methods = BTreeSet::new();
    for method in reference_methods.keys() {
        if let Some(projection) = behaviour::siginfo_projection(method) {
            native.push((
                format!("siginfo_{method}"),
                behaviour::siginfo(method, projection),
            ));
            checked_methods.insert(method.clone());
            asserter.ledger.count("inherent_method_behaviour_rt");
        }
    }

    let mut out: Vec<String> = [
        "// Copyright 2026 Andrew Yates",
        "// SPDX-License-Identifier: Apache-2.0",
        "//",
    ]
    .map(String::from)
    .into();
    out.push(format!(
        "// GENERATED by `aterm-libc-oracle regen` from the reference libc {REFERENCE_VERSION}"
    ));
    out.push(format!(
        "// rustdoc for {}: {} declared items. Do not edit by hand.",
        cell.triple,
        decls.set.len()
    ));
    out.push("#![allow(non_upper_case_globals, non_camel_case_types, unused_parens)]".into());
    out.push("#![allow(deprecated)]".into());
    out.push(String::new());
    out.append(&mut asserter.lines);
    out.push(String::new());
    out.push("// ---- behaviour of the C macros libc reimplements as Rust code ----".into());
    out.extend(evaluated);
    out.push(String::new());
    out.push("// ---- runtime checks: only execute on the native cell ----".into());
    out.push("#[cfg(test)]".into());
    out.push("mod runtime {".into());
    let runtime_tests = asserter.runtime.len() + native.len();
    for (name, code) in &asserter.runtime {
        out.push(format!(
            "    #[test] fn const_{}() {{ {code} }}",
            name.to_lowercase()
        ));
    }
    for (name, code) in &native {
        out.push(format!(
            "    #[test] fn behaviour_{}() {{ {code} }}",
            name.to_lowercase()
        ));
    }
    out.push("}".into());
    let mut text = out.join("\n");
    text.push('\n');

    let mut audit = coverage(decls, &asserter.ledger);
    let expected_macros: BTreeSet<String> = handwritten::macros(cell.os)
        .iter()
        .map(|m| m.name)
        .filter(|name| declares(name))
        .map(String::from)
        .collect();
    let emitted: BTreeSet<String> =
        handwritten::public_unsafe_fns(handwritten::siginfo_impls(cell.os))
            .into_iter()
            .collect();
    let reference_methods: BTreeSet<String> = reference_methods.into_keys().collect();
    audit.extend(handwritten_gaps(
        &expected_macros,
        &checked_macros,
        &reference_methods,
        &emitted,
        &checked_methods,
    ));
    let ledger = asserter.ledger;
    Ok(Generated {
        text,
        checked_items: ledger.checked.values().filter(|k| !k.is_empty()).count(),
        checked_members: ledger.members.len(),
        stats: ledger.stats,
        runtime_tests,
        audit,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::regen::decls;
    use crate::rustdoc::fixture::doc;

    fn path(name: &str, id: u64) -> String {
        format!(r#"{{"resolved_path":{{"path":"{name}","id":{id},"args":null}}}}"#)
    }

    /// `K: c_int`; `P: *mut c_void`; `s { x: t, y: e }` with `t = u32`; `e` a
    /// one-variant enum; `f(e) -> c_int`; `g(t)`; `n = *mut s` (a nominal alias).
    fn fixture() -> String {
        let index = [
            format!(r#""1":{{"id":1,"name":"K","inner":{{"constant":{{"type":{},"const":{{"expr":"2","value":"2i32"}}}}}}}}"#, path("c_int", 90)),
            format!(r#""2":{{"id":2,"name":"P","inner":{{"constant":{{"type":{{"raw_pointer":{{"is_mutable":true,"type":{}}}}},"const":{{"expr":"_","value":null}}}}}}}}"#, path("c_void", 91)),
            r#""3":{"id":3,"name":"s","inner":{"struct":{"kind":{"plain":{"fields":[4,5],"has_stripped_fields":true}},"impls":[]}}}"#.into(),
            format!(r#""4":{{"id":4,"name":"x","inner":{{"struct_field":{}}}}}"#, path("t", 6)),
            format!(r#""5":{{"id":5,"name":"y","inner":{{"struct_field":{}}}}}"#, path("e", 7)),
            r#""6":{"id":6,"name":"t","inner":{"type_alias":{"type":{"primitive":"u32"}}}}"#.into(),
            r#""7":{"id":7,"name":"e","attrs":[{"repr":{"kind":"rust","align":null,"packed":null,"int":"u32"}}],"inner":{"enum":{"variants":[8],"has_stripped_variants":false,"impls":[]}}}"#.into(),
            r#""8":{"id":8,"name":"A","inner":{"variant":{"kind":"plain","discriminant":{"expr":"1","value":"1"}}}}"#.into(),
            format!(r#""9":{{"id":9,"name":"f","inner":{{"function":{{"sig":{{"inputs":[["v",{}]],"output":{},"is_c_variadic":false}},"header":{{"is_const":false,"is_unsafe":true,"abi":{{"C":{{"unwind":false}}}}}},"has_body":false}}}}}}"#, path("e", 7), path("c_int", 90)),
            format!(r#""10":{{"id":10,"name":"g","inner":{{"function":{{"sig":{{"inputs":[["v",{}]],"output":null,"is_c_variadic":true}},"header":{{"is_const":false,"is_unsafe":true,"abi":{{"C":{{"unwind":false}}}}}},"has_body":false}}}}}}"#, path("t", 6)),
            format!(r#""11":{{"id":11,"name":"n","inner":{{"type_alias":{{"type":{{"raw_pointer":{{"is_mutable":true,"type":{}}}}}}}}}}}"#, path("s", 3)),
            r#""12":{"id":12,"name":"siginfo_t","inner":{"struct":{"kind":{"plain":{"fields":[],"has_stripped_fields":true}},"impls":[13]}}}"#.into(),
            r#""13":{"id":13,"inner":{"impl":{"trait":null,"is_synthetic":false,"blanket_impl":null,"is_negative":false,"items":[14]}}}"#.into(),
            format!(r#""14":{{"id":14,"name":"si_pid","visibility":"public","inner":{{"function":{{"sig":{{"inputs":[["self",{{"borrowed_ref":{{"lifetime":null,"is_mutable":false,"type":{{"generic":"Self"}}}}}}]],"output":{},"is_c_variadic":false}},"header":{{"is_const":false,"is_unsafe":true,"abi":"Rust"}},"has_body":true}}}}}}"#, path("c_int", 90)),
        ]
        .join(",");
        doc(&[1, 2, 3, 6, 7, 9, 10, 11, 12], &index, "")
    }

    fn generate(union: &[&str]) -> Generated {
        let parsed = Doc::parse(&fixture()).expect("parses");
        let public = parsed.public().expect("public");
        let union: Vec<String> = union.iter().map(|n| (*n).to_string()).collect();
        let decls = decls::read(&parsed, &public, &union).expect("declarations");
        emit(&crate::cells::CELLS[0], &parsed, &public, &decls).expect("emits")
    }

    #[test]
    fn each_kind_is_pinned_in_its_strongest_form() {
        let generated = generate(&[
            "K",
            "P",
            "f",
            "g",
            "n",
            "c_int",
            "c_void",
            "gone",
            "siginfo_t",
        ]);
        let text = &generated.text;
        let triple = "aarch64-apple-darwin";
        for want in [
            // A transparent constant shares one type pin; its value is compared.
            "const _: libc_real::c_int = libc_shim::K;".to_string(),
            format!("const _: () = assert!(libc_shim::K as i128 == libc_real::K as i128, \"libc constant K differs on {triple}\");"),
            // A transparent signature coerces to ONE fn-pointer type...
            "const _: unsafe extern \"C\" fn(libc_real::t, ...) = libc_shim::g;".into(),
            // ...a nominal one is mirrored, each crate in its own types.
            "const _: unsafe extern \"C\" fn(libc_shim::e) -> libc_shim::c_int = libc_shim::f;".into(),
            "const _: unsafe extern \"C\" fn(libc_real::e) -> libc_real::c_int = libc_real::f;".into(),
            // A field of alias type shares its type; one of enum type mirrors.
            "const _: fn(libc_shim::s) -> libc_real::t = |v| v.x;".into(),
            "const _: fn(libc_shim::s) -> libc_shim::e = |v| v.y;".into(),
            "const _: fn(libc_real::s) -> libc_real::e = |v| v.y;".into(),
            "// s: has private fields; size_of/align_of are the only public contract for them".into(),
            format!("const _: () = assert!(libc_shim::e::A as i128 == libc_real::e::A as i128, \"libc e::A: discriminant differs on {triple}\");"),
            // A nominal alias is pinned both ways inside each crate.
            "const _: fn(libc_shim::n) -> *mut libc_shim::s = |x| x;".into(),
            "const _: fn(*mut libc_real::s) -> libc_real::n = |x| x;".into(),
            "const _: fn(libc_shim::c_int) -> libc_real::c_int = |x| x;".into(),
            format!("const _: () = assert!(::core::mem::size_of::<libc_shim::c_void>() == ::core::mem::size_of::<libc_real::c_void>(), \"libc c_void: size_of differs on {triple}\");"),
            "// !! gone: ABSENT from oracle libc on this cell".into(),
            // A pointer constant is compared where it can be evaluated: natively.
            "    #[test] fn const_p() { assert_eq!(libc_shim::P as usize, libc_real::P as usize, \"libc pointer constant P differs\"); }".into(),
            // Each public inherent accessor's signature, pinned per crate.
            "const _: unsafe fn(&libc_shim::siginfo_t) -> libc_shim::c_int = libc_shim::siginfo_t::si_pid;".into(),
            "const _: unsafe fn(&libc_real::siginfo_t) -> libc_real::c_int = libc_real::siginfo_t::si_pid;".into(),
        ] {
            assert!(text.lines().any(|line| line == want), "missing: {want}\n{text}");
        }
        assert!(text.contains("    #[test] fn behaviour_siginfo_si_pid() { unsafe {"));
        // The reference here has one accessor; the Darwin family emits five.
        assert_eq!(
            generated.audit,
            [
                "EMITTED METHODS NOT IN REFERENCE: {\"si_addr\", \"si_status\", \"si_uid\", \"si_value\"}"
            ]
        );
        assert_eq!(generated.stats["fn_shared"], 1);
        assert_eq!(generated.stats["fn_mirrored"], 1);
        assert_eq!(generated.stats["inherent_method"], 1);
        assert_eq!(generated.runtime_tests, 2);
        assert!(text.starts_with("// Copyright 2026 Andrew Yates\n"));
    }

    /// The audit is computed against what the shim DECLARES: an item the oracle
    /// did not check, or a check of an undeclared item, fails it.
    #[test]
    fn the_audit_diffs_the_ledger_against_the_declarations() {
        let parsed = Doc::parse(&fixture()).expect("parses");
        let public = parsed.public().expect("public");
        let union = ["f".to_string(), "siginfo_t".into()];
        let mut decls = decls::read(&parsed, &public, &union).expect("declarations");
        let clean = emit(&crate::cells::CELLS[0], &parsed, &public, &decls).expect("emits");
        assert_eq!(clean.audit.len(), 1, "only the fixture's missing accessors");
        // A field the oracle never saw, declared on the shim's side.
        decls.structs.push(crate::regen::decls::Record {
            name: "s".into(),
            repr: None,
            stripped: false,
            traits: vec![],
            fields: vec![crate::regen::decls::Field {
                name: "z".into(),
                ty: "u8".into(),
            }],
        });
        let audit = emit(&crate::cells::CELLS[0], &parsed, &public, &decls)
            .expect("emits")
            .audit;
        assert_eq!(
            audit[..3],
            [
                "THE SHIM DECLARES A DIFFERENT SET THAN THE ORACLE CHECKS: {\"s\"}",
                "DECLARED BUT NOT CHECKED: {\"s\"}",
                "FIELDS DECLARED BUT NOT CHECKED: {\"s.z\"}",
            ]
        );
    }

    #[test]
    fn a_macro_without_a_sweep_or_an_unswept_accessor_is_a_gap() {
        let set = |names: &[&str]| -> BTreeSet<String> {
            names.iter().map(|n| (*n).to_string()).collect()
        };
        assert!(
            handwritten_gaps(
                &set(&["A"]),
                &set(&["A"]),
                &set(&["m"]),
                &set(&["m"]),
                &set(&["m"])
            )
            .is_empty()
        );
        assert_eq!(
            handwritten_gaps(
                &set(&["A", "B"]),
                &set(&["A"]),
                &set(&["m", "n"]),
                &set(&["m", "o"]),
                &set(&["m"])
            ),
            [
                "MACROS WITHOUT BEHAVIOR: {\"B\"}",
                "REFERENCE METHODS NOT EMITTED: {\"n\"}",
                "EMITTED METHODS NOT IN REFERENCE: {\"o\"}",
                "METHODS WITHOUT BEHAVIOR: {\"n\"}",
            ]
        );
    }
}
