// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
        "const _: libc_real::c_int = libc_shim::K;".to_string(),
        format!("const _: () = assert!(libc_shim::K as i128 == libc_real::K as i128, \"libc constant K differs on {triple}\");"),
        "const _: unsafe extern \"C\" fn(libc_real::t, ...) = libc_shim::g;".into(),
        "const _: unsafe extern \"C\" fn(libc_shim::e) -> libc_shim::c_int = libc_shim::f;".into(),
        "const _: unsafe extern \"C\" fn(libc_real::e) -> libc_real::c_int = libc_real::f;".into(),
        "const _: fn(libc_shim::s) -> libc_real::t = |v| v.x;".into(),
        "const _: fn(libc_shim::s) -> libc_shim::e = |v| v.y;".into(),
        "const _: fn(libc_real::s) -> libc_real::e = |v| v.y;".into(),
        "// s: has private fields; size_of/align_of are the only public contract for them".into(),
        format!("const _: () = assert!(libc_shim::e::A as i128 == libc_real::e::A as i128, \"libc e::A: discriminant differs on {triple}\");"),
        "const _: fn(libc_shim::n) -> *mut libc_shim::s = |x| x;".into(),
        "const _: fn(*mut libc_real::s) -> libc_real::n = |x| x;".into(),
        "const _: fn(libc_shim::c_int) -> libc_real::c_int = |x| x;".into(),
        format!("const _: () = assert!(::core::mem::size_of::<libc_shim::c_void>() == ::core::mem::size_of::<libc_real::c_void>(), \"libc c_void: size_of differs on {triple}\");"),
        "// !! gone: ABSENT from oracle libc on this cell".into(),
        "    #[test] fn const_p() { assert_eq!(libc_shim::P as usize, libc_real::P as usize, \"libc pointer constant P differs\"); }".into(),
        "const _: unsafe fn(&libc_shim::siginfo_t) -> libc_shim::c_int = libc_shim::siginfo_t::si_pid;".into(),
        "const _: unsafe fn(&libc_real::siginfo_t) -> libc_real::c_int = libc_real::siginfo_t::si_pid;".into(),
    ] {
        assert!(text.lines().any(|line| line == want), "missing: {want}\n{text}");
    }
    assert!(text.contains("    #[test] fn behaviour_siginfo_si_pid() { unsafe {"));
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
    let set =
        |names: &[&str]| -> BTreeSet<String> { names.iter().map(|n| (*n).to_string()).collect() };
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
