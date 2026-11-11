// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_cells_list_is_read_by_its_key() {
    let union = Union::parse(r#"{"mac":["stat","AF_INET"],"linux":[1]}"#);
    assert_eq!(union, Err("a non-string name in `linux`".into()));
    let union = Union::parse(r#"{"mac":["stat","AF_INET"]}"#).expect("parses");
    assert_eq!(
        union.names(&CELLS[0]),
        Ok(vec!["AF_INET".to_string(), "stat".to_string()])
    );
    assert!(union.names(&CELLS[1]).is_err());
    assert!(Union::parse(r#"{"win":[]}"#).is_err());
    assert!(Union::parse("[]").is_err());
}

#[test]
fn widening_adds_only_new_names_and_renders_in_table_order() {
    let mut union = Union::parse(r#"{"linux":["b"],"mac":["a"]}"#).expect("parses");
    let added = union.widen(&CELLS[0], ["c".to_string(), "a".to_string()]);
    assert_eq!(added, ["c"]);
    let canonical = "{\n \"mac\": [\n  \"a\",\n  \"c\"\n ],\n \"linux\": [\n  \"b\"\n ]\n}\n";
    assert_eq!(union.render(), canonical);
    let reread = Union::parse(canonical).expect("parses");
    assert_eq!(reread.render(), canonical);
}
