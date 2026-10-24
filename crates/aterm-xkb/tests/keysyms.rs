// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm-xkb's keysyms against xkeysym 0.2.1's.

use std::collections::BTreeMap;

use aterm_xkb::keysym::NAMES;

fn recorded() -> BTreeMap<String, u32> {
    let path = format!(
        "{}/tests/recorded/xkeysym-0.2.1-keysyms.tsv",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{path}: {e}"))
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            let (name, value) = l.split_once('\t').expect("<name>\t<value>");
            let value = u32::from_str_radix(value.trim_start_matches("0x"), 16).expect("hex");
            (name.to_owned(), value)
        })
        .collect()
}

#[test]
fn every_keysym_is_xkeysyms() {
    let want = recorded();
    let got: BTreeMap<String, u32> = NAMES
        .iter()
        .map(|(n, _, v)| ((*n).to_owned(), *v))
        .collect();
    assert_eq!(want.len(), 218, "the corpus is whole");
    assert_eq!(got.len(), NAMES.len(), "no name twice");
    assert_eq!(got, want);
}

#[test]
fn a_value_one_away_misses() {
    let want = recorded();
    let (name, _, value) = NAMES[0];
    assert_ne!(want.get(name), Some(&(value + 1)));
    assert_eq!(want.get(name), Some(&value));
}
