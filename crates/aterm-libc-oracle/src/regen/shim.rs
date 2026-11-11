// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The shim's cell module, `crates/aterm-libc/src/<cell>.rs`, written from a
//! cell's declarations ([`super::decls`]) and measurements ([`super::probe`]).

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
    let has_digit = number.bytes().any(|b| b.is_ascii_digit());
    let suffixed = suffix.is_empty() || INT_SUFFIXES.contains(&suffix);
    (has_digit && suffixed).then(|| format!("{sign}{}", number.replace('_', "")))
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
    if packed == Some(0) {
        return Err(format!("{name}: packed(0) is invalid"));
    }
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
        let eff_align = packed.map_or(field_align, |p| field_align.min(p));
        let natural = up(pos, eff_align).ok_or_else(|| {
            format!(
                "cannot place {name}.{}: alignment {eff_align} or offset {pos} is invalid",
                field.name
            )
        })?;
        if natural < want {
            if !want.is_multiple_of(eff_align) {
                return Err(format!(
                    "cannot place {name}.{}: measured {want} is not aligned to {eff_align}",
                    field.name
                ));
            }
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
        pos = pos
            .checked_add(field_size)
            .ok_or_else(|| format!("{name}.{}: extent overflowed u64", field.name))?;
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
    let mut types = std::borrow::Cow::Borrowed(&measured.field_types);
    let mut notes = Vec::new();
    let slots: Vec<Slot> = if !record.stripped {
        record
            .fields
            .iter()
            .map(|f| (f.name.clone(), f.ty.clone(), true))
            .collect()
    } else if record.fields.is_empty() {
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
        types
            .to_mut()
            .insert(format!("[{unit}; {}]", size / align), (size, align));
        vec![(
            "__opaque".into(),
            format!("[{unit}; {}]", size / align),
            false,
        )]
    } else {
        let slots = padded(record, measured, size, types.to_mut())?;
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
        ]
        .map(String::from),
    );
    out.push(aterm_census::seal::line("//", super::GENERATOR));
    out.push(String::new());
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
    structs.sort_unstable_by(|a, b| a.name.cmp(&b.name));
    for record in structs {
        out.extend(struct_lines(os, record, measured)?);
        out.push(String::new());
    }
    let mut unions: Vec<&Record> = decls.unions.iter().collect();
    unions.sort_unstable_by(|a, b| a.name.cmp(&b.name));
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
    consts.sort_unstable_by(|a, b| a.name.cmp(&b.name));
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
    fns.sort_unstable_by(|a, b| a.name.cmp(&b.name));
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
    out.push(format!(
        "{}{};",
        crate::siblings::BODY_MARKER,
        handwritten::fd_word(cell.os)
    ));
    out.push(String::new());
    {
        let word = if cell.os == Os::Darwin {
            "u32"
        } else {
            "usize"
        };
        out.extend(
            [
                "/// Control messages follow this ABI's control-message alignment.",
                "const fn cmsg_align(len: usize) -> usize {",
                &format!("    (len + ::core::mem::size_of::<{word}>() - 1) & !(::core::mem::size_of::<{word}>() - 1)"),
                "}",
                "",
            ]
            .map(String::from),
        );
    }
    let mut fns: Vec<&Func> = decls.fns.iter().filter(|f| f.has_body).collect();
    fns.sort_unstable_by(|a, b| a.name.cmp(&b.name));
    for func in fns {
        let body = handwritten::macro_body(cell.os, &func.name)
            .ok_or_else(|| format!("no body written for {} on {}", func.name, cell.key))?;
        if let Some(doc) = handwritten::safety_doc(cell.os, &func.name).filter(|_| func.is_unsafe) {
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
#[path = "shim_tests.rs"]
mod tests;
