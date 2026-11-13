// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::fixture::{doc, function};
use super::*;

fn names(public: &Public<'_>) -> Vec<(String, Namespace, String)> {
    public
        .iter()
        .flat_map(|(name, by_ns)| {
            by_ns
                .iter()
                .map(|(ns, e)| ((*name).to_string(), *ns, e.kind.to_string()))
        })
        .collect()
}

#[test]
fn a_name_can_be_both_a_type_and_a_function() {
    let text = doc(
        &[1, 2],
        &[
            function(1, "stat", false, true, ""),
            r#""2":{"id":2,"name":"stat","inner":{"struct":{"kind":{"plain":{"fields":[]}},"impls":[]}}}"#
                .to_string(),
        ]
        .join(","),
        "",
    );
    let parsed = Doc::parse(&text).expect("parses");
    assert_eq!(
        names(&parsed.public().expect("public")),
        [
            ("stat".into(), Namespace::Type, "struct".into()),
            ("stat".into(), Namespace::Value, "function".into()),
        ]
    );
}

#[test]
fn globs_are_walked_renames_resolve_and_modules_are_not_items() {
    let index = [
        r#""1":{"id":1,"inner":{"use":{"source":"self::unix","name":"unix","id":3,"is_glob":true}}}"#,
        r#""2":{"id":2,"inner":{"use":{"source":"x","name":"y","id":5,"is_glob":false}}}"#,
        r#""3":{"id":3,"name":"unix","inner":{"module":{"items":[4,6]}}}"#,
        &function(4, "getpid", false, true, ""),
        r#""5":{"id":5,"name":"x","inner":{"constant":{"type":{"primitive":"i32"},"const":{"expr":"1"}}}}"#,
        r#""6":{"id":6,"name":"inner","inner":{"module":{"items":[7]}}}"#,
        &function(7, "hidden", false, true, ""),
        r#""8":{"id":8,"inner":{"use":{"source":"m","name":"m","id":6,"is_glob":false}}}"#,
    ]
    .join(",");
    let parsed = Doc::parse(&doc(&[1, 2, 6, 8], &index, "")).expect("parses");
    assert_eq!(
        names(&parsed.public().expect("public")),
        [
            ("getpid".into(), Namespace::Value, "function".into()),
            ("y".into(), Namespace::Value, "constant".into()),
        ]
    );
}

#[test]
fn the_first_item_a_name_meets_wins() {
    let index = [
        function(1, "f", false, true, ""),
        function(2, "f", true, false, ""),
    ]
    .join(",");
    let parsed = Doc::parse(&doc(&[1, 2], &index, "")).expect("parses");
    let public = parsed.public().expect("public");
    let entry = public["f"][&Namespace::Value];
    assert_eq!(entry.item.get("id").and_then(Value::as_u64), Some(1));
}

#[test]
fn a_document_without_an_index_or_root_is_a_format_error() {
    assert!(Doc::parse("{}").is_err());
    assert!(Doc::parse(r#"{"index":{}}"#).is_err());
    assert!(Doc::parse("not json").is_err());
    let rootless = Doc::parse(r#"{"index":{},"root":9}"#).expect("parses");
    assert!(rootless.public().is_err());
}
