// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The shim's cell module, `crates/aterm-libc/src/<cell>.rs`, written from a
//! cell's declarations ([`super::decls`]) and measurements ([`super::probe`]).
//!
//! Every value is derived: constants and signatures from the reference's
//! rustdoc, sizes, offsets and the bytes of the struct-valued initialisers from
//! the probes. Where libc keeps private fields, byte padding reproduces their
//! measured extent, placed by the layout engine ([`super::layout`]). The only
//! hand-written code comes from [`super::handwritten`].

use std::fmt::Write as _;

use super::decls::{Const, Decls, Func, Record, Repr};
use super::handwritten;
use super::layout::{TypeSizes, layout, up};
use super::probe::Measured;
use crate::cells::{Cell, Os};
use crate::closure::PRIMS;

const KEYWORDS: [&str; 29] = [
    "type", "ref", "match", "move", "box", "fn", "mod", "use", "in", "as", "if", "else", "loop",
    "while", "for", "let", "const", "static", "struct", "enum", "impl", "trait", "self", "super",
    "crate", "where", "pub", "return", "yield",
];

const INT_SUFFIXES: [&str; 12] = [
    "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize",
];

/// `digits` is one or more `digit` characters, each optionally followed by
/// underscores, starting with a digit.
fn digits(text: &str, digit: impl Fn(char) -> bool) -> bool {
    let mut chars = text.chars();
    chars.next().is_some_and(&digit) && chars.all(|c| digit(c) || c == '_')
}

/// An integer literal: decimal, `0x`, `0o` or `0b`, optionally negated.
fn is_int_literal(text: &str) -> bool {
    let text = text.strip_prefix('-').unwrap_or(text);
    let radix = |prefix: &str, digit: fn(char) -> bool| {
        text.strip_prefix(prefix)
            .is_some_and(|rest| digits(rest, digit))
    };
    radix("0x", |c| c.is_ascii_hexdigit())
        || radix("0o", |c| matches!(c, '0'..='7'))
        || radix("0b", |c| matches!(c, '0' | '1'))
        || digits(text, |c| c.is_ascii_digit())
}

/// A rustdoc-evaluated pointer value, `{0x10 as *mut core::ffi::c_void}`.
fn pointer_value(value: &str) -> Option<String> {
    let inner = value
        .strip_prefix('{')?
        .strip_suffix(" as *mut core::ffi::c_void}")?;
    let number = match inner.strip_prefix("0x") {
        Some(hex) if !hex.is_empty() && hex.chars().all(|c| c.is_ascii_hexdigit()) => {
            u128::from_str_radix(hex, 16).ok()?
        }
        Some(_) => return None,
        None if !inner.is_empty() && inner.chars().all(|c| c.is_ascii_digit()) => {
            inner.parse().ok()?
        }
        None => return None,
    };
    Some(if number == 0 {
        "core::ptr::null_mut()".into()
    } else {
        format!("{inner} as *mut c_void")
    })
}

/// A rustdoc-evaluated integer, `-1i32` or `4_294_967_295u32`, unsuffixed.
fn integer_value(value: &str) -> Option<String> {
    let (sign, unsigned) = match value.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", value),
    };
    let end = unsigned
        .find(|c: char| !(c.is_ascii_digit() || c == '_'))
        .unwrap_or(unsigned.len());
    let (number, suffix) = unsigned.split_at(end);
    let suffixed = suffix.is_empty() || INT_SUFFIXES.contains(&suffix);
    (!number.is_empty() && suffixed).then(|| format!("{sign}{}", number.replace('_', "")))
}

/// A constant's value as Rust source; `None` when only its bytes can state it
/// (a struct-valued initialiser, read by the probes).
///
/// # Errors
/// On an evaluated value of a shape this does not write.
pub fn const_expr(constant: &Const) -> Result<Option<String>, String> {
    if let Some(expr) = constant
        .expr
        .as_deref()
        .filter(|e| *e != "_" && is_int_literal(e))
    {
        return Ok(Some(expr.into()));
    }
    let Some(value) = constant.value.as_deref() else {
        return Ok(None);
    };
    pointer_value(value)
        .or_else(|| integer_value(value))
        .map(Some)
        .ok_or_else(|| format!("unhandled constant value {value:?} for {}", constant.name))
}

/// An argument's name, renamed when rustdoc omitted it or it is a keyword.
fn arg_name(name: &str, index: usize) -> String {
    if name.is_empty() || name == "_" {
        format!("arg{index}")
    } else if KEYWORDS.contains(&name) {
        format!("{name}_")
    } else {
        name.into()
    }
}

fn args(func: &Func) -> String {
    let args: Vec<String> = func
        .args
        .iter()
        .enumerate()
        .map(|(i, arg)| format!("{}: {}", arg_name(&arg.name, i), arg.ty))
        .collect();
    args.join(", ")
}

fn ret(func: &Func) -> String {
    func.ret
        .as_ref()
        .map_or_else(String::new, |ty| format!(" -> {ty}"))
}

/// The derives reproducing exactly the reference's explicit impls; the
/// comparison traits only under `extra_traits`, as upstream gates them.
fn trait_lines(traits: &[&str]) -> Vec<String> {
    let pick = |set: [&'static str; 3]| -> Vec<&'static str> {
        set.into_iter().filter(|t| traits.contains(t)).collect()
    };
    let base = pick(["Clone", "Copy", "Debug"]);
    let extra = pick(["PartialEq", "Eq", "Hash"]);
    let mut out = Vec::new();
    if !base.is_empty() {
        out.push(format!("#[derive({})]", base.join(", ")));
    }
    if !extra.is_empty() {
        out.push(format!(
            "#[cfg_attr(feature = \"extra_traits\", derive({}))]",
            extra.join(", ")
        ));
    }
    out
}

fn measured_of(measured: &Measured, name: &str) -> Result<(u64, u64), String> {
    let size = measured.size.get(name);
    let align = measured.align.get(name);
    size.zip(align)
        .map(|(s, a)| (*s, *a))
        .ok_or_else(|| format!("{name}: its size and alignment were not measured"))
}

/// One emitted field: name, type, public.
type Slot = (String, String, bool);

/// The fields that reproduce a struct libc keeps private fields in: the public
/// ones at their measured offsets, `__pad<n>` byte arrays between and after.
fn padded(
    record: &Record,
    measured: &Measured,
    size: u64,
    types: &mut TypeSizes,
) -> Result<Vec<Slot>, String> {
    let name = &record.name;
    let packed = record.repr.as_ref().and_then(|r| r.packed);
    let mut slots = Vec::new();
    let (mut pos, mut pads) = (0, 0);
    let mut pad = |slots: &mut Vec<Slot>, bytes: u64, types: &mut TypeSizes| {
        let ty = format!("[u8; {bytes}]");
        types.insert(ty.clone(), (bytes, 1));
        slots.push((format!("__pad{pads}"), ty, false));
        pads += 1;
    };
    for field in &record.fields {
        let key = (name.clone(), field.name.clone());
        let want = *measured
            .offset
            .get(&key)
            .ok_or_else(|| format!("{name}.{}: its offset was not measured", field.name))?;
        let (field_size, field_align) = *types
            .get(&field.ty)
            .ok_or_else(|| format!("no measured size for field type `{}`", field.ty))?;
        let natural = up(pos, packed.map_or(field_align, |p| field_align.min(p)));
        if natural < want {
            pad(&mut slots, want - pos, types);
            pos = want;
        } else if natural > want {
            return Err(format!(
                "cannot place {name}.{}: natural {natural} > measured {want}",
                field.name
            ));
        } else {
            pos = natural;
        }
        slots.push((field.name.clone(), field.ty.clone(), true));
        pos += field_size;
    }
    if size > pos {
        pad(&mut slots, size - pos, types);
    }
    Ok(slots)
}

/// One struct: its notes, `repr`, derives and fields.
fn struct_lines(os: Os, record: &Record, measured: &Measured) -> Result<Vec<String>, String> {
    let name = &record.name;
    let (size, align) = measured_of(measured, name)?;
    let repr = record.repr.clone().unwrap_or_default();
    let Repr {
        packed,
        align: mut forced,
        ..
    } = repr;
    let mut types = measured.field_types.clone();
    let mut notes = Vec::new();
    let slots: Vec<Slot> = if !record.stripped {
        record
            .fields
            .iter()
            .map(|f| (f.name.clone(), f.ty.clone(), true))
            .collect()
    } else if record.fields.is_empty() {
        // Fully opaque in libc: size and alignment ARE the whole public contract.
        let unit = match align {
            8 => "u64",
            4 => "u32",
            2 => "u16",
            1 => "u8",
            _ => return Err(format!("{name}: no integer of alignment {align}")),
        };
        if size % align != 0 {
            return Err(format!("{name}: {size} bytes is not a multiple of {align}"));
        }
        notes.push(format!("opaque in libc: {size} bytes, align {align}"));
        types.insert(format!("[{unit}; {}]", size / align), (size, align));
        vec![(
            "__opaque".into(),
            format!("[{unit}; {}]", size / align),
            false,
        )]
    } else {
        let slots = padded(record, measured, size, &mut types)?;
        notes.push(format!(
            "libc keeps private fields here; the __pad members reproduce their measured \
             extent (total {size} bytes, align {align})"
        ));
        slots
    };
    let fields: Vec<(&str, &str)> = slots
        .iter()
        .map(|(n, t, _)| (n.as_str(), t.as_str()))
        .collect();
    let (_, mut computed_size, mut computed_align) = layout(&fields, &types, packed, forced)?;
    if record.stripped && !record.fields.is_empty() && computed_align < align {
        // libc reaches this alignment through a private field of a wider type;
        // state it directly instead of inventing one.
        forced = Some(align);
        (_, computed_size, computed_align) = layout(&fields, &types, packed, forced)?;
        notes.push(
            "alignment stated explicitly: libc gets it from a private field whose type is \
             not part of the contract"
                .into(),
        );
    }
    let opaque = record.stripped && record.fields.is_empty();
    if !opaque && (computed_size, computed_align) != (size, align) {
        return Err(format!(
            "{name}: emitted layout {computed_size}/{computed_align}, measured {size}/{align}"
        ));
    }
    let mut out: Vec<String> = notes.iter().map(|n| format!("/// {n}")).collect();
    if let Some(note) = handwritten::struct_note(os, name) {
        out.extend(
            note.split('\n')
                .map(|line| format!("/// {line}").trim_end().to_string()),
        );
    }
    let mut reprs = vec!["C".to_string()];
    reprs.extend(packed.map(|p| format!("packed({p})")));
    reprs.extend(forced.map(|a| format!("align({a})")));
    out.push(format!("#[repr({})]", reprs.join(", ")));
    out.extend(trait_lines(&record.traits));
    // A derived `PartialEq` compares function pointers by address, which rustc
    // warns about; upstream allows the lint on the same structs, and matching it
    // keeps the comparison semantics of the crate this replaces.
    if record.traits.contains(&"PartialEq") && slots.iter().any(|(_, ty, _)| ty.contains("fn(")) {
        out.push("#[allow(unpredictable_function_pointer_comparisons)]".into());
    }
    out.push(format!("pub struct {name} {{"));
    for (field, ty, public) in &slots {
        let vis = if *public { "pub " } else { "" };
        out.push(format!("    {vis}{field}: {ty},"));
    }
    out.push("}".into());
    Ok(out)
}

/// The module's header: what it is, and how it is made.
fn header(cell: &Cell, out: &mut Vec<String>) {
    out.extend(
        [
            "// Copyright 2026 Andrew Yates",
            "// SPDX-License-Identifier: Apache-2.0",
            "",
        ]
        .map(String::from),
    );
    out.push(format!("//! `libc` for `{}`.", cell.triple));
    out.extend(
        [
            "//!",
            "//! GENERATED, and deliberately: every value below is a fact about this",
            "//! platform's ABI, measured for THIS TRIPLE and checked against the",
            "//! conformance oracle in `libc-oracle/` (see `src/lib.rs` for the method;",
            "//! regenerate with `targo --unverified run -q -p aterm-libc-oracle -- regen`).",
            "//! The only hand-written code in this file is the bodies of the C macros",
            "//! libc reimplements in Rust and the inherent siginfo_t accessors. The",
            "//! oracle audits their exact sets and checks their behavior against the",
            "//! reference libc.",
            "",
            "#[allow(unused_imports)]",
            "use core::ffi::{",
        ]
        .map(String::from),
    );
    for chunk in PRIMS.chunks(6) {
        out.push(format!("    {},", chunk.join(", ")));
    }
    out.push("};".into());
    out.push(String::new());
}

fn types(os: Os, decls: &Decls, measured: &Measured, out: &mut Vec<String>) -> Result<(), String> {
    out.push("// ---------------------------------------------------------------- types".into());
    for alias in &decls.aliases {
        out.push(format!("pub type {} = {};", alias.name, alias.ty));
    }
    out.push(String::new());
    for e in &decls.enums {
        if e.variants.is_empty() {
            out.push("/// Opaque to callers: libc exposes this as an uninhabited type.".into());
            out.extend(trait_lines(&e.traits));
            out.push(format!("pub enum {} {{}}", e.name));
        } else {
            let int = e
                .repr
                .as_ref()
                .and_then(|r| r.int.as_deref())
                .ok_or_else(|| format!("{}: an inhabited enum without an integer repr", e.name))?;
            out.push(format!("#[repr({int})]"));
            out.extend(trait_lines(&e.traits));
            out.push(format!("pub enum {} {{", e.name));
            for variant in &e.variants {
                let value = variant
                    .expr
                    .as_deref()
                    .filter(|x| !x.is_empty())
                    .or(variant.value.as_deref())
                    .ok_or_else(|| format!("{}::{}: no discriminant", e.name, variant.name))?;
                out.push(format!("    {} = {value},", variant.name));
            }
            out.push("}".into());
        }
        out.push(String::new());
    }
    let mut structs: Vec<&Record> = decls.structs.iter().collect();
    structs.sort_by(|a, b| a.name.cmp(&b.name));
    for record in structs {
        out.extend(struct_lines(os, record, measured)?);
        out.push(String::new());
    }
    let mut unions: Vec<&Record> = decls.unions.iter().collect();
    unions.sort_by(|a, b| a.name.cmp(&b.name));
    for union in unions {
        if union.stripped {
            return Err(format!("{}: a union with private fields", union.name));
        }
        let (size, align) = measured_of(measured, &union.name)?;
        out.push(format!("/// C union: {size} bytes, align {align}."));
        out.push("#[repr(C)]".into());
        out.extend(trait_lines(&union.traits));
        out.push(format!("pub union {} {{", union.name));
        for field in &union.fields {
            out.push(format!("    pub {}: {},", field.name, field.ty));
        }
        out.push("}".into());
        out.push(String::new());
    }
    Ok(())
}

fn constants(
    cell: &Cell,
    decls: &Decls,
    measured: &Measured,
    out: &mut Vec<String>,
) -> Result<(), String> {
    out.push("// ------------------------------------------------------------ constants".into());
    let mut consts: Vec<&Const> = decls.consts.iter().collect();
    consts.sort_by(|a, b| a.name.cmp(&b.name));
    let mut by_bytes = Vec::new();
    for constant in consts {
        match const_expr(constant)? {
            Some(value) => out.push(format!(
                "pub const {}: {} = {value};",
                constant.name, constant.ty
            )),
            None => by_bytes.push(constant),
        }
    }
    for constant in by_bytes {
        let (name, ty) = (&constant.name, &constant.ty);
        let bytes = measured
            .bytes
            .get(name)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let count = bytes.len();
        out.push(String::new());
        out.push(format!(
            "/// {name} is a struct-valued initializer whose contents are opaque in"
        ));
        out.push(format!(
            "/// libc. Its {count} bytes were read out of the platform ABI by a"
        ));
        out.push(format!(
            "/// const-eval probe compiled for {} and are asserted byte-exact",
            cell.triple
        ));
        out.push("/// against the oracle.".into());
        out.push(format!("pub const {name}: {ty} = unsafe {{"));
        out.push(format!(
            "    ::core::mem::transmute::<[u8; {count}], {ty}>(["
        ));
        for row in bytes.chunks(12) {
            let mut line = String::from("       ");
            for byte in row {
                let _ = write!(line, " 0x{byte:02x},");
            }
            out.push(line);
        }
        out.push("    ])".into());
        out.push("};".into());
    }
    out.push(String::new());
    Ok(())
}

fn functions(decls: &Decls, out: &mut Vec<String>) {
    out.push("// ------------------------------------------------------------ functions".into());
    out.push("unsafe extern \"C\" {".into());
    let mut fns: Vec<&Func> = decls.fns.iter().filter(|f| !f.has_body).collect();
    fns.sort_by(|a, b| a.name.cmp(&b.name));
    for func in fns {
        let mut args = args(func);
        if func.variadic {
            args.push_str(", ...");
        }
        if let Some(link_name) = &func.link_name {
            out.push(format!("    #[link_name = \"{link_name}\"]"));
        }
        out.push(format!("    pub fn {}({args}){};", func.name, ret(func)));
    }
    out.push("}".into());
    out.push(String::new());
}

fn bodies(cell: &Cell, decls: &Decls, out: &mut Vec<String>) -> Result<(), String> {
    out.extend(
        [
            "// ------------------------------------------------------------------------",
            "// The C macros. These and the siginfo_t accessors below are the two",
            "// hand-written body classes: real code with bit-exact semantics for which",
            "// declaration and signature equality alone are not enough.",
            "// The oracle covers every body: status helpers over -1..=0xFFFF plus",
            "// signed/high-bit extremes, device helpers over deterministic samples,",
            "// and pointer-writing/ancillary-data helpers with native runtime tests.",
            "// ------------------------------------------------------------------------",
            "/// The word `fd_set` is a bitmap of. Selecting it from the measured",
            "/// alignment rather than naming a C type keeps the bit order pinned to",
            "/// the same layout the oracle diffs byte-for-byte.",
        ]
        .map(String::from),
    );
    out.push(format!("type fd_word = {};", handwritten::fd_word(cell.os)));
    out.push(String::new());
    if cell.os == Os::GnuLinux {
        out.extend(
            [
                "/// Control messages are padded out to a pointer-sized boundary.",
                "const fn cmsg_align(len: usize) -> usize {",
                "    (len + ::core::mem::size_of::<usize>() - 1) & !(::core::mem::size_of::<usize>() - 1)",
                "}",
                "",
            ]
            .map(String::from),
        );
    }
    let mut fns: Vec<&Func> = decls.fns.iter().filter(|f| f.has_body).collect();
    fns.sort_by(|a, b| a.name.cmp(&b.name));
    for func in fns {
        let body = handwritten::macro_body(cell.os, &func.name)
            .ok_or_else(|| format!("no body written for {} on {}", func.name, cell.key))?;
        if let Some(doc) = handwritten::safety_doc(&func.name).filter(|_| func.is_unsafe) {
            out.extend(["/// # Safety", "///"].map(String::from));
            out.extend(doc.iter().map(|line| format!("/// {line}")));
        }
        out.push("#[inline]".into());
        out.push(format!(
            "pub {}{}extern \"C\" fn {}({}){} {{",
            if body.is_const { "const " } else { "" },
            if func.is_unsafe { "unsafe " } else { "" },
            func.name,
            args(func),
            ret(func),
        ));
        out.extend(body.body.split('\n').map(|line| format!("    {line}")));
        out.push("}".into());
        out.push(String::new());
    }
    out.extend(
        [
            "// ------------------------------------------------------------------------",
            "// Inherent methods libc puts on these types. Reinterpret-casts over the",
            "// private payload, each carrying its own repr(C) prefix struct.",
            "// ------------------------------------------------------------------------",
        ]
        .map(String::from),
    );
    out.extend(
        handwritten::siginfo_impls(cell.os)
            .split('\n')
            .map(String::from),
    );
    Ok(())
}

/// The cell's module text, before formatting.
///
/// # Errors
/// When a declaration cannot be written from what was read and measured.
pub fn emit(cell: &Cell, decls: &Decls, measured: &Measured) -> Result<String, String> {
    let mut out = Vec::new();
    header(cell, &mut out);
    types(cell.os, decls, measured, &mut out)?;
    constants(cell, decls, measured, &mut out)?;
    functions(decls, &mut out);
    bodies(cell, decls, &mut out)?;
    let mut text = out.join("\n").trim_end().to_string();
    text.push('\n');
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cells::CELLS;
    use crate::regen::decls::{Arg, Field};

    fn constant(expr: Option<&str>, value: Option<&str>) -> Const {
        Const {
            name: "K".into(),
            ty: "c_int".into(),
            expr: expr.map(String::from),
            value: value.map(String::from),
        }
    }

    #[test]
    fn constants_keep_literal_source_else_write_the_evaluated_value() {
        let expr = |e, v| const_expr(&constant(e, v));
        assert_eq!(
            expr(Some("0x7f_ff"), Some("32767i32")),
            Ok(Some("0x7f_ff".into()))
        );
        assert_eq!(expr(Some("-0o17"), None), Ok(Some("-0o17".into())));
        assert_eq!(expr(Some("1 << 3"), Some("8i32")), Ok(Some("8".into())));
        assert_eq!(expr(Some("_"), Some("-4_294i64")), Ok(Some("-4294".into())));
        assert_eq!(expr(Some("_"), Some("12")), Ok(Some("12".into())));
        assert_eq!(
            expr(None, Some("{0x0 as *mut core::ffi::c_void}")),
            Ok(Some("core::ptr::null_mut()".into()))
        );
        assert_eq!(
            expr(None, Some("{0xffffffffffffffff as *mut core::ffi::c_void}")),
            Ok(Some("0xffffffffffffffff as *mut c_void".into()))
        );
        assert_eq!(expr(Some("_"), None), Ok(None), "only bytes can state it");
        assert!(expr(None, Some("1.5f64")).is_err());
        assert!(expr(None, Some("8i31")).is_err());
        assert!(!is_int_literal("0x") && !is_int_literal("_1") && is_int_literal("1_000"));
    }

    #[test]
    fn keyword_and_unnamed_arguments_are_renamed() {
        assert_eq!(
            [
                arg_name("type", 0),
                arg_name("", 1),
                arg_name("_", 2),
                arg_name("fd", 3)
            ],
            ["type_", "arg1", "arg2", "fd"]
        );
    }

    #[test]
    fn derives_reproduce_the_explicit_impls_with_comparisons_feature_gated() {
        assert_eq!(
            trait_lines(&["Clone", "Copy", "Debug", "PartialEq", "Eq", "Hash"]),
            [
                "#[derive(Clone, Copy, Debug)]",
                "#[cfg_attr(feature = \"extra_traits\", derive(PartialEq, Eq, Hash))]"
            ]
        );
        assert_eq!(trait_lines(&["Hash", "Copy"]).len(), 2);
        assert!(trait_lines(&[]).is_empty());
    }

    fn measured() -> Measured {
        let mut m = Measured::default();
        for (ty, size) in [("u8", 1), ("u32", 4), ("c_long", 8)] {
            m.field_types.insert(ty.into(), (size, size));
        }
        m
    }

    fn record(name: &str, stripped: bool, fields: &[(&str, &str)]) -> Record {
        Record {
            name: name.into(),
            repr: None,
            stripped,
            traits: vec!["Clone", "Copy"],
            fields: fields
                .iter()
                .map(|(n, t)| Field {
                    name: (*n).into(),
                    ty: (*t).into(),
                })
                .collect(),
        }
    }

    #[test]
    fn private_fields_become_measured_padding_and_alignment_is_stated() {
        let mut m = measured();
        m.size.insert("s".into(), 24);
        m.align.insert("s".into(), 8);
        m.offset.insert(("s".into(), "a".into()), 4);
        m.offset.insert(("s".into(), "b".into()), 12);
        let lines = struct_lines(
            Os::Darwin,
            &record("s", true, &[("a", "u8"), ("b", "u32")]),
            &m,
        )
        .expect("emits");
        assert_eq!(
            lines,
            [
                "/// libc keeps private fields here; the __pad members reproduce their measured extent (total 24 bytes, align 8)",
                "/// alignment stated explicitly: libc gets it from a private field whose type is not part of the contract",
                "#[repr(C, align(8))]",
                "#[derive(Clone, Copy)]",
                "pub struct s {",
                "    __pad0: [u8; 4],",
                "    pub a: u8,",
                "    __pad1: [u8; 7],",
                "    pub b: u32,",
                "    __pad2: [u8; 8],",
                "}",
            ]
        );
        // A public field measured before where it could start is unplaceable.
        m.offset.insert(("s".into(), "b".into()), 6);
        assert!(
            struct_lines(
                Os::Darwin,
                &record("s", true, &[("a", "u8"), ("b", "u32")]),
                &m
            )
            .is_err()
        );
    }

    #[test]
    fn an_opaque_struct_is_its_size_in_words_and_a_public_one_must_match() {
        let mut m = measured();
        m.size.insert("o".into(), 64);
        m.align.insert("o".into(), 8);
        let lines = struct_lines(Os::GnuLinux, &record("o", true, &[]), &m).expect("emits");
        assert_eq!(lines[0], "/// opaque in libc: 64 bytes, align 8");
        assert_eq!(lines[lines.len() - 2], "    __opaque: [u64; 8],");
        m.size.insert("p".into(), 16);
        m.align.insert("p".into(), 8);
        let public = record("p", false, &[("a", "u32"), ("b", "c_long")]);
        assert!(struct_lines(Os::GnuLinux, &public, &m).is_ok());
        m.size.insert("p".into(), 24);
        assert!(
            struct_lines(Os::GnuLinux, &public, &m).is_err(),
            "the engine disagrees"
        );
    }

    fn arg(name: &str, ty: &str) -> Arg {
        Arg {
            name: name.into(),
            ty: ty.into(),
        }
    }

    /// Each kind in its section, in section order: aliases, enums, structs and
    /// unions; literal, evaluated and byte-stated constants (twelve bytes a
    /// row); externs with their link names; the macro bodies.
    #[test]
    fn a_cell_module_writes_each_kind_in_its_section() {
        use crate::regen::decls::{Alias, Enum, Variant};
        let mut m = measured();
        for (name, size) in [("p", 16), ("u", 4)] {
            m.size.insert(name.into(), size);
            m.align.insert(name.into(), size.min(8));
        }
        m.bytes
            .insert("PTHREAD_MUTEX_INITIALIZER".into(), (0..14).collect());
        let int = |name: &str, expr: Option<&str>, value: Option<&str>| Const {
            name: name.into(),
            ..constant(expr, value)
        };
        let decls = Decls {
            aliases: vec![Alias {
                name: "pid_t".into(),
                ty: "i32".into(),
            }],
            enums: vec![
                Enum {
                    name: "DIR".into(),
                    repr: None,
                    variants: vec![],
                    traits: vec![],
                },
                Enum {
                    name: "e".into(),
                    repr: Some(Repr {
                        int: Some("u32".into()),
                        ..Repr::default()
                    }),
                    variants: vec![
                        Variant {
                            name: "A".into(),
                            value: Some("1".into()),
                            expr: Some("0x1".into()),
                        },
                        Variant {
                            name: "B".into(),
                            value: Some("2".into()),
                            expr: None,
                        },
                    ],
                    traits: vec!["Clone"],
                },
            ],
            structs: vec![record("p", false, &[("a", "u32"), ("b", "c_long")])],
            unions: vec![Record {
                traits: vec![],
                ..record("u", false, &[("x", "u32")])
            }],
            consts: vec![
                int("N", Some("_"), Some("-1i32")),
                int("K", Some("0x10"), Some("16i32")),
                Const {
                    name: "PTHREAD_MUTEX_INITIALIZER".into(),
                    ty: "pthread_mutex_t".into(),
                    expr: Some("_".into()),
                    value: None,
                },
            ],
            fns: vec![
                Func {
                    name: "open".into(),
                    is_unsafe: true,
                    variadic: true,
                    args: vec![
                        arg("path", "*const c_char"),
                        arg("type", "c_int"),
                        arg("", "c_int"),
                    ],
                    ret: Some("c_int".into()),
                    link_name: Some("open$X".into()),
                    has_body: false,
                },
                Func {
                    name: "WSTOPSIG".into(),
                    is_unsafe: false,
                    variadic: false,
                    args: vec![arg("status", "c_int")],
                    ret: Some("c_int".into()),
                    link_name: None,
                    has_body: true,
                },
            ],
            ..Decls::default()
        };
        let text = emit(&CELLS[0], &decls, &m).expect("emits");
        let declarations = "\
// ---------------------------------------------------------------- types
pub type pid_t = i32;

/// Opaque to callers: libc exposes this as an uninhabited type.
pub enum DIR {}

#[repr(u32)]
#[derive(Clone)]
pub enum e {
    A = 0x1,
    B = 2,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct p {
    pub a: u32,
    pub b: c_long,
}

/// C union: 4 bytes, align 4.
#[repr(C)]
pub union u {
    pub x: u32,
}

// ------------------------------------------------------------ constants
pub const K: c_int = 0x10;
pub const N: c_int = -1;

/// PTHREAD_MUTEX_INITIALIZER is a struct-valued initializer whose contents are opaque in
/// libc. Its 14 bytes were read out of the platform ABI by a
/// const-eval probe compiled for aarch64-apple-darwin and are asserted byte-exact
/// against the oracle.
pub const PTHREAD_MUTEX_INITIALIZER: pthread_mutex_t = unsafe {
    ::core::mem::transmute::<[u8; 14], pthread_mutex_t>([
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b,
        0x0c, 0x0d,
    ])
};

// ------------------------------------------------------------ functions
unsafe extern \"C\" {
    #[link_name = \"open$X\"]
    pub fn open(path: *const c_char, type_: c_int, arg2: c_int, ...) -> c_int;
}
";
        assert!(text.contains(declarations), "{text}");
        let body = "\
type fd_word = u32;

#[inline]
pub const extern \"C\" fn WSTOPSIG(status: c_int) -> c_int {
    status >> 8
}
";
        assert!(text.contains(body), "{text}");
        assert!(text.ends_with("}\n") && !text.ends_with("\n\n"));
    }

    #[test]
    fn a_macro_without_a_written_body_stops_the_cell() {
        let decls = Decls {
            fns: vec![Func {
                name: "WNOWHERE".into(),
                is_unsafe: false,
                variadic: false,
                args: vec![Arg {
                    name: "status".into(),
                    ty: "c_int".into(),
                }],
                ret: Some("bool".into()),
                link_name: None,
                has_body: true,
            }],
            ..Decls::default()
        };
        let error = emit(&CELLS[0], &decls, &measured()).expect_err("no body");
        assert_eq!(error, "no body written for WNOWHERE on mac");
    }
}
