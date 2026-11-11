// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::rustdoc::Doc;
use crate::rustdoc::fixture::{doc, function};

fn symbols_of(index: &str, items: &[u64]) -> BTreeMap<String, String> {
    let parsed = Doc::parse(&doc(items, index, "")).expect("parses");
    extern_symbols(&parsed.public().expect("public")).expect("symbols")
}

#[test]
fn a_link_name_attribute_is_the_symbol_and_a_body_has_none() {
    let index = [
        function(
            1,
            "stat",
            false,
            true,
            r##"{"other":"#[attr = LinkName {name: \"stat$INODE64\"}]"}"##,
        ),
        function(
            2,
            "getpid",
            false,
            true,
            r##"{"other":"#[allow(deprecated)]"}"##,
        ),
        function(3, "WIFEXITED", true, false, ""),
        function(4, "old", false, true, r##""#[link_name = \"old$1050\"]""##),
    ]
    .join(",");
    let symbols = symbols_of(&index, &[1, 2, 3, 4]);
    let got: Vec<(&str, &str)> = symbols
        .iter()
        .map(|(n, s)| (n.as_str(), s.as_str()))
        .collect();
    assert_eq!(
        got,
        [
            ("getpid", "getpid"),
            ("old", "old$1050"),
            ("stat", "stat$INODE64")
        ]
    );
    assert_eq!(redirected(&symbols), 2);
}

#[test]
fn a_dropped_link_name_and_an_extra_declaration_are_diffs() {
    let reference: BTreeMap<String, String> = [
        ("stat".to_string(), "stat$INODE64".to_string()),
        ("getpid".to_string(), "getpid".to_string()),
    ]
    .into();
    let shim: BTreeMap<String, String> = [
        ("stat".to_string(), "stat".to_string()),
        ("getpid".to_string(), "getpid".to_string()),
        ("made_up".to_string(), "made_up".to_string()),
    ]
    .into();
    assert_eq!(
        diff(&reference, &shim),
        [
            SymbolDiff::AbsentFromReference {
                name: "made_up".into()
            },
            SymbolDiff::Mismatch {
                name: "stat".into(),
                shim: "stat".into(),
                reference: "stat$INODE64".into()
            },
        ]
    );
    assert!(diff(&reference, &reference).is_empty());
}
