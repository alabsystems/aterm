// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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

#[test]
fn public_inherent_signatures_extend_the_transitive_declaration_set() {
    for owner in [
        r#"{"struct":{"kind":{"plain":{"fields":[]}},"impls":[2,3]}}"#,
        r#"{"union":{"fields":[],"impls":[2,3]}}"#,
        r#"{"enum":{"variants":[],"impls":[2,3]}}"#,
    ] {
        let index = r#"
            "1":{"id":1,"name":"owner","inner":@OWNER@},
            "2":{"id":2,"inner":{"impl":{"trait":null,"items":[4,5]}}},
            "3":{"id":3,"inner":{"impl":{"trait":{"path":"Unrelated"},"items":[6]}}},
            "4":{"id":4,"name":"public_method","visibility":"public","inner":{"function":{"sig":{
                "inputs":[["arg",{"resolved_path":{"path":"method_input","id":7,"args":null}}]],
                "output":{"resolved_path":{"path":"clock_t","id":8,"args":null}}
            }}}},
            "5":{"id":5,"name":"private_method","visibility":"default","inner":{"function":{"sig":{
                "inputs":[],"output":{"resolved_path":{"path":"private_only","id":10,"args":null}}
            }}}},
            "6":{"id":6,"name":"trait_method","visibility":"public","inner":{"function":{"sig":{
                "inputs":[],"output":{"resolved_path":{"path":"trait_only","id":11,"args":null}}
            }}}},
            "7":{"id":7,"name":"method_input","inner":{"type_alias":{"type":{"primitive":"i32"}}}},
            "8":{"id":8,"name":"clock_t","inner":{"type_alias":{"type":{"resolved_path":{"path":"ticks","id":9,"args":null}}}}},
            "9":{"id":9,"name":"ticks","inner":{"type_alias":{"type":{"primitive":"i64"}}}},
            "10":{"id":10,"name":"private_only","inner":{"type_alias":{"type":{"primitive":"i32"}}}},
            "11":{"id":11,"name":"trait_only","inner":{"type_alias":{"type":{"primitive":"i32"}}}}
        "#
        .replace("@OWNER@", owner);
        let parsed = Doc::parse(&doc(&[1, 7, 8, 9, 10, 11], &index, "")).expect("parses");
        let public = parsed.public().expect("public");
        let reached = closure(&parsed, &public, &["owner".into()]).expect("closure");
        let reached: Vec<&str> = reached.iter().map(String::as_str).collect();
        assert_eq!(reached, ["clock_t", "method_input", "owner", "ticks"]);
    }
}
