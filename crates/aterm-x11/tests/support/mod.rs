// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Reading the recordings under tests/recorded/.

#![allow(dead_code, reason = "each module uses its own part")]

use std::collections::BTreeMap;

/// Parses tab-separated rows from `tests/recorded/{name}`.
pub fn rows(name: &str) -> Vec<Vec<String>> {
    let path = format!("{}/tests/recorded/{name}", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    text.lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.split('\t').map(str::to_owned).collect())
        .collect()
}

/// Rows keyed by their first field, which must be unique.
pub fn by_case(name: &str) -> BTreeMap<String, Vec<String>> {
    let mut out = BTreeMap::new();
    for row in rows(name) {
        let key = row[0].clone();
        assert!(
            out.insert(key.clone(), row[1..].to_vec()).is_none(),
            "{name}: case {key} recorded twice"
        );
    }
    out
}

pub fn unhex(s: &str) -> Vec<u8> {
    assert!(s.len().is_multiple_of(2), "odd hex: {s}");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// `1` / `0`, as the recorder writes a bool.
pub fn b(v: bool) -> u8 {
    u8::from(v)
}

/// Asserts that `got` matches `recorded` for every case.
pub fn same_cases(what: &str, recorded: &BTreeMap<String, String>, got: &BTreeMap<String, String>) {
    let missing: Vec<_> = recorded.keys().filter(|k| !got.contains_key(*k)).collect();
    let extra: Vec<_> = got.keys().filter(|k| !recorded.contains_key(*k)).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "{what}: every recording needs a case and every case a recording; \
         recorded but not built: {missing:?}; built but not recorded: {extra:?}"
    );
    let mut wrong = Vec::new();
    for (case, want) in recorded {
        if &got[case] != want {
            wrong.push(format!(
                "{case}:\n  recorded {want}\n  aterm    {}",
                got[case]
            ));
        }
    }
    assert!(wrong.is_empty(), "{what}: {}", wrong.join("\n"));
}
