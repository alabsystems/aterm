// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A module built by hand: types, one import, two functions, a memory,
/// exports and a custom section.
pub(crate) fn sample(custom: &[(&str, &[u8])]) -> Vec<u8> {
    fn section(out: &mut Vec<u8>, id: u8, payload: &[u8]) {
        out.push(id);
        leb(out, payload.len() as u32);
        out.extend(payload);
    }
    let mut m = MAGIC.to_vec();
    section(&mut m, TYPE, &[2, 0x60, 2, 0x7f, 0x7e, 1, 0x7c, 0x60, 0, 0]);
    section(&mut m, IMPORT, &[1, 3, b'e', b'n', b'v', 1, b'f', 0, 1]);
    section(&mut m, FUNCTION, &[2, 0, 1]);
    section(&mut m, 5, &[1, 0, 1]);
    let mut e = vec![4];
    for (name, k, i) in [
        ("memory", 2u8, 0u8),
        ("g", 0, 1),
        ("h", 0, 2),
        ("__data_end", 3, 0),
    ] {
        e.push(name.len() as u8);
        e.extend(name.as_bytes());
        e.extend([k, i]);
    }
    section(&mut m, EXPORT, &e);
    for (name, bytes) in custom {
        let mut c = vec![name.len() as u8];
        c.extend(name.as_bytes());
        c.extend(*bytes);
        section(&mut m, CUSTOM, &c);
    }
    section(&mut m, 10, &[2, 2, 0, 0x0b, 2, 0, 0x0b]);
    m
}

#[test]
fn the_reader_sees_types_imports_exports_and_custom_sections() {
    let bytes = sample(&[
        ("aterm_abi", b"one"),
        ("other", b"x"),
        ("aterm_abi", b"two"),
    ]);
    let m = Module::parse(&bytes).unwrap();
    assert_eq!(m.imports[0].module, "env");
    assert_eq!(m.imports[0].ty, Some(1));
    assert_eq!(m.funcs, [0, 1]);
    let names: Vec<&str> = m.exports.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["memory", "g", "h", "__data_end"]);
    assert_eq!(
        m.func_type(1),
        Some(&FuncType {
            params: vec![0x7f, 0x7e],
            results: vec![0x7c]
        })
    );
    assert_eq!(
        m.func_type(0),
        m.func_type(2),
        "the import and h share type 1"
    );
    assert_eq!(m.custom("aterm_abi"), [b"one".as_slice(), b"two"]);
}

#[test]
fn a_rewrite_drops_only_what_it_names() {
    let bytes = sample(&[("aterm_abi", b"rec"), ("keep", b"k")]);
    let m = Module::parse(&bytes).unwrap();
    let out = m.rewrite(&["aterm_abi"], &["__data_end"]);
    let r = Module::parse(&out).unwrap();
    assert!(r.custom("aterm_abi").is_empty());
    assert_eq!(r.custom("keep"), [b"k".as_slice()]);
    let names: Vec<&str> = r.exports.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["memory", "g", "h"]);
    assert_eq!(r.funcs, m.funcs);
    assert_eq!(m.rewrite(&[], &[]), bytes);
}

#[test]
fn a_foreign_or_cut_file_is_refused() {
    assert!(Module::parse(b"\0asm\x02\0\0\0").is_err());
    let bytes = sample(&[]);
    assert!(Module::parse(&bytes[..bytes.len() - 2]).is_err());

    let mut oversized_funcs = MAGIC.to_vec();
    oversized_funcs.extend([FUNCTION, 2, 0xff, 0x01]);
    assert!(Module::parse(&oversized_funcs).is_err());

    let mut trailing_funcs = MAGIC.to_vec();
    trailing_funcs.extend([FUNCTION, 3, 1, 0, 99]);
    assert!(Module::parse(&trailing_funcs).is_err());

    let mut dup_func_section = MAGIC.to_vec();
    dup_func_section.extend([FUNCTION, 2, 1, 0, FUNCTION, 2, 1, 0]);
    assert!(Module::parse(&dup_func_section).is_err());

    let mut dup_export = MAGIC.to_vec();
    dup_export.extend([EXPORT, 9, 2, 1, b'g', 0, 0, 1, b'g', 0, 1]);
    assert!(Module::parse(&dup_export).is_err());
}
