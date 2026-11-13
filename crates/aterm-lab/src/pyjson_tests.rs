// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn spells_as_python_json_dumps_does() {
    let v = J::obj([
        ("a", J::Int(1)),
        ("b", J::Arr(vec![J::s("x"), J::Null])),
        ("c", J::obj([])),
    ]);
    assert_eq!(v.py(), r#"{"a": 1, "b": ["x", null], "c": {}}"#);
    assert_eq!(v.dump(COMPACT), r#"{"a":1,"b":["x",null],"c":{}}"#);
    let pretty = v.dump(Style {
        indent: Some(2),
        compact: false,
        ascii: true,
    });
    assert_eq!(
        pretty,
        "{\n  \"a\": 1,\n  \"b\": [\n    \"x\",\n    null\n  ],\n  \"c\": {}\n}"
    );
    assert_eq!(J::s("é❯").py(), r#""\u00e9\u276f""#);
    assert_eq!(
        J::s("é").dump(Style {
            indent: None,
            compact: false,
            ascii: false
        }),
        "\"é\""
    );
}

#[test]
fn floats_read_as_python_repr() {
    assert_eq!(py_float(2.0), "2.0");
    assert_eq!(py_float(1_792_091_818.268_117), "1792091818.268117");
    assert_eq!(py_float(0.414), "0.414");
    assert_eq!(py_float(1e16), "1e+16");
    assert_eq!(py_float(1.5e-5), "1.5e-05");
}

#[test]
fn merge_removes_on_null_merges_objects_and_replaces_lists() {
    let mut base =
        J::parse(r#"{"env":{"A":"1"},"permissions":{"allow":["x"]},"apiKeyHelper":"/k"}"#).unwrap();
    let patch =
        J::parse(r#"{"apiKeyHelper":null,"env":{"B":"2"},"permissions":{"allow":["y"]}}"#).unwrap();
    merge(&mut base, &patch);
    assert_eq!(base.get("apiKeyHelper"), None);
    assert_eq!(base.get("env").unwrap().py(), r#"{"A": "1", "B": "2"}"#);
    assert_eq!(base.get("permissions").unwrap().py(), r#"{"allow": ["y"]}"#);
    let mut dup = J::obj([("k", J::obj([("a", J::Int(1))])), ("k", J::Int(2))]);
    dup.set("k", J::Int(3));
    assert_eq!(dup.get("k"), Some(&J::Int(3)));
    assert_eq!(dup.py(), r#"{"k": 3}"#);
    let mut dup_obj = J::obj([("k", J::Int(1)), ("k", J::obj([("a", J::Int(2))]))]);
    dup_obj.entry_obj("k").set("b", J::Int(3));
    assert_eq!(dup_obj.py(), r#"{"k": {"a": 2, "b": 3}}"#);
}
