// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Synthetic rustdoc documents for the unit tests.

/// Fixture rustdoc JSON document with root module `items`.
pub fn doc(items: &[u64], index: &str, paths: &str) -> String {
    let items: Vec<String> = items.iter().map(u64::to_string).collect();
    let sep = if index.is_empty() { "" } else { "," };
    format!(
        r#"{{"root":0,"format_version":60,"paths":{{{paths}}},"index":{{
                "0":{{"id":0,"name":"krate","visibility":"public","attrs":[],
                      "inner":{{"module":{{"items":[{}],"is_crate":true}}}}}}{sep}{index}}}}}"#,
        items.join(",")
    )
}

/// An index member for a public function.
pub fn function(id: u64, name: &str, has_body: bool, is_unsafe: bool, attrs: &str) -> String {
    format!(
        r#""{id}":{{"id":{id},"name":"{name}","visibility":"public","attrs":[{attrs}],
                "inner":{{"function":{{"sig":{{"inputs":[],"output":null,"is_c_variadic":false}},
                "header":{{"is_const":false,"is_unsafe":{is_unsafe},"is_async":false,
                           "abi":{{"C":{{"unwind":false}}}}}},
                "has_body":{has_body}}}}}}}"#
    )
}
