// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::rustdoc::fixture::doc;

fn path(name: &str, id: u64) -> String {
    format!(r#"{{"resolved_path":{{"path":"{name}","id":{id},"args":null}}}}"#)
}

fn fixture() -> String {
    let index = [
        format!(
            r##""1":{{"id":1,"name":"f","attrs":[{{"other":"#[attr = LinkName {{name: \"f$X\"}}]"}}],"inner":{{"function":{{"sig":{{"inputs":[["p",{{"raw_pointer":{{"is_mutable":true,"type":{}}}}}]],"output":{},"is_c_variadic":false}},"header":{{"is_unsafe":true,"abi":{{"C":{{"unwind":false}}}}}},"has_body":false}}}}}}"##,
            path("s", 2),
            path("c_int", 90)
        ),
        r#""2":{"id":2,"name":"s","attrs":[{"repr":{"kind":"c","align":null,"packed":4,"int":null}}],"inner":{"struct":{"kind":{"plain":{"fields":[5],"has_stripped_fields":false}},"impls":[10,11,12]}}}"#.into(),
        r#""3":{"id":3,"name":"t","inner":{"type_alias":{"type":{"primitive":"u32"}}}}"#.into(),
        r#""4":{"id":4,"name":"K","inner":{"constant":{"type":{"primitive":"i32"},"const":{"expr":"_","value":"8i32","is_literal":false}}}}"#.into(),
        format!(r#""5":{{"id":5,"name":"x","inner":{{"struct_field":{}}}}}"#, path("t", 3)),
        r#""6":{"id":6,"name":"e","attrs":[{"repr":{"kind":"rust","align":null,"packed":null,"int":"u32"}}],"inner":{"enum":{"variants":[7],"has_stripped_variants":false,"impls":[]}}}"#.into(),
        r#""7":{"id":7,"name":"A","inner":{"variant":{"kind":"plain","discriminant":{"expr":"1","value":"1"}}}}"#.into(),
        r#""10":{"id":10,"inner":{"impl":{"trait":{"path":"Hash","id":20,"args":null},"is_synthetic":false,"blanket_impl":null,"is_negative":false,"items":[]}}}"#.into(),
        r#""11":{"id":11,"inner":{"impl":{"trait":{"path":"Send","id":21,"args":null},"is_synthetic":true,"blanket_impl":null,"is_negative":false,"items":[]}}}"#.into(),
        r#""12":{"id":12,"inner":{"impl":{"trait":{"path":"core::clone::Clone","id":22,"args":null},"is_synthetic":false,"blanket_impl":null,"is_negative":false,"items":[]}}}"#.into(),
    ]
    .join(",");
    doc(&[1, 2, 3, 4, 6], &index, "")
}

#[test]
fn a_cells_declarations_are_read_by_kind_with_the_closure() {
    let parsed = Doc::parse(&fixture()).expect("parses");
    let public = parsed.public().expect("public");
    let union = [
        "K".to_string(),
        "c_int".into(),
        "e".into(),
        "f".into(),
        "gone".into(),
    ];
    let decls = read(&parsed, &public, &union).expect("reads");
    assert_eq!(decls.closure_only, ["s", "t"]);
    assert_eq!(decls.prims, ["c_int"]);
    assert_eq!(decls.absent, ["gone"]);
    assert_eq!(
        decls.consts,
        [Const {
            name: "K".into(),
            ty: "i32".into(),
            expr: Some("_".into()),
            value: Some("8i32".into()),
        }]
    );
    assert_eq!(
        decls.fns,
        [Func {
            name: "f".into(),
            is_unsafe: true,
            variadic: false,
            args: vec![Arg {
                name: "p".into(),
                ty: "*mut s".into(),
            }],
            ret: Some("c_int".into()),
            link_name: Some("f$X".into()),
            has_body: false,
        }]
    );
    assert_eq!(
        decls.structs,
        [Record {
            name: "s".into(),
            repr: Some(Repr {
                align: None,
                packed: Some(4),
                int: None,
            }),
            stripped: false,
            traits: vec!["Clone", "Hash"],
            fields: vec![Field {
                name: "x".into(),
                ty: "t".into(),
            }],
        }]
    );
    assert_eq!(
        decls.enums[0].repr.as_ref().and_then(|r| r.int.as_deref()),
        Some("u32")
    );
    assert_eq!(
        decls.enums[0].variants,
        [Variant {
            name: "A".into(),
            value: Some("1".into()),
            expr: Some("1".into()),
        }]
    );
    assert_eq!(decls.aliases[0].ty, "u32");
    let declared: Vec<String> = decls.declared().into_iter().collect();
    assert_eq!(declared, ["K", "c_int", "e", "f", "s", "t"]);
    let members: Vec<(String, String)> = decls.members().into_iter().collect();
    assert_eq!(
        members,
        [("e".to_string(), "A".to_string()), ("s".into(), "x".into())]
    );
}

#[test]
fn a_generic_trait_impl_or_an_unmodelled_type_is_refused() {
    let index = [
        r#""2":{"id":2,"name":"s","inner":{"struct":{"kind":{"plain":{"fields":[],"has_stripped_fields":false}},"impls":[10]}}}"#,
        r#""10":{"id":10,"inner":{"impl":{"trait":{"path":"PartialEq","id":20,"args":{"angle_bracketed":{"args":[],"constraints":[]}}},"is_synthetic":false,"blanket_impl":null,"is_negative":false,"items":[]}}}"#,
        r#""3":{"id":3,"name":"t","inner":{"type_alias":{"type":{"generic":"T"}}}}"#,
    ]
    .join(",");
    let parsed = Doc::parse(&doc(&[2, 3], &index, "")).expect("parses");
    let public = parsed.public().expect("public");
    assert!(read(&parsed, &public, &["s".into()]).is_err_and(|e| e.contains("cannot derive")));
    assert!(read(&parsed, &public, &["t".into()]).is_err_and(|e| e.contains("unmodelled")));
}
