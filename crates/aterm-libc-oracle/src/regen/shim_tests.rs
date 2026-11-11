// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
    assert!(expr(None, Some("_")).is_err());
    assert!(expr(None, Some("__i32")).is_err());
    assert!(expr(None, Some("-_")).is_err());
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
    m.offset.insert(("s".into(), "b".into()), 6);
    assert!(
        struct_lines(
            Os::Darwin,
            &record("s", true, &[("a", "u8"), ("b", "u32")]),
            &m
        )
        .is_err()
    );
    m.offset.insert(("s".into(), "b".into()), 10);
    assert!(
        struct_lines(
            Os::Darwin,
            &record("s", true, &[("a", "u8"), ("b", "u32")]),
            &m
        )
        .is_err(),
        "misaligned offset past natural placement is refused"
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

/// Control messages follow this ABI's control-message alignment.
const fn cmsg_align(len: usize) -> usize {
    (len + ::core::mem::size_of::<u32>() - 1) & !(::core::mem::size_of::<u32>() - 1)
}

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

/// The sibling gate finds a cell's hand-written section by a line of code
/// this generator writes, so a comment prune of the cell sources leaves
/// each section defined and the gate's verdict unchanged.
#[test]
fn a_comment_prune_leaves_each_sibling_section_and_the_verdict() {
    use crate::siblings::{SiblingFinding, compare};
    use std::collections::BTreeMap;
    let prune = |text: &str| -> String {
        text.lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .map(|line| format!("{line}\n"))
            .collect()
    };
    let emitted: BTreeMap<&str, String> = CELLS
        .iter()
        .map(|cell| {
            let text = emit(cell, &Decls::default(), &measured()).expect("an empty cell");
            (cell.source, text)
        })
        .collect();
    let pruned: BTreeMap<&str, String> = emitted
        .iter()
        .map(|(file, text)| (*file, prune(text)))
        .collect();
    assert_ne!(emitted, pruned, "the generator writes comments to prune");
    assert_eq!(compare(&emitted), []);
    assert_eq!(compare(&pruned), [], "a comment prune changed the verdict");
    let mut planted = pruned;
    let x86 = planted.get_mut("darwin_x86_64.rs").expect("the cell");
    x86.push_str("const fn planted() {}\n");
    assert!(
        compare(&planted)
            .iter()
            .any(|f| matches!(f, SiblingFinding::Differs { os: Os::Darwin, .. })),
        "{:?}",
        compare(&planted)
    );
}
