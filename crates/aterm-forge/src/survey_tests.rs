// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn fit_truncates_by_chars_and_never_splits_one() {
    assert_eq!(fit("abc", 5), "abc");
    assert_eq!(fit("abcdef", 4), "abc…");
    assert_eq!(fit("ünïcödé-crate", 5), "ünïc…");
    assert_eq!(fit("abc", 0), "");
}

#[test]
fn wrap_respects_w_line_for_short_tokens_and_preserves_words() {
    let words: Vec<String> = (0..40).map(|i| format!("pkg_{i:02}_v1.2.3")).collect();
    let input = format!("{}\n{}", words[..25].join(" "), words[25..].join("  "));
    let mut out = String::new();
    wrap(&mut out, "    ", &input, W_LINE);
    for line in out.lines() {
        assert!(
            line.chars().count() <= W_LINE,
            "wrapped line exceeded W_LINE ({W_LINE}): len={} {line:?}",
            line.chars().count()
        );
    }
    let round_trip: Vec<&str> = out.split_whitespace().collect();
    let expected: Vec<&str> = words.iter().map(String::as_str).collect();
    assert_eq!(round_trip, expected);
}

#[test]
fn unmeasured_third_party_sources_skip_the_cell_instead_of_reporting_zero_loc() {
    use crate::model::{Graph, PkgFacts};
    let root_id = PkgId::new("aterm", "0.1.0");
    let missing_id = PkgId::new("unpacked-dep", "1.2.3");
    let present_id = PkgId::new("vendored-dep", "0.4.0");

    let make_cell = |name: &str, dep: &PkgId, dir: Option<std::path::PathBuf>| {
        let mut graph = Graph {
            root: root_id.clone(),
            ..Graph::default()
        };
        graph.nodes.insert(root_id.clone());
        graph.nodes.insert(dep.clone());
        graph
            .edges
            .entry(root_id.clone())
            .or_default()
            .insert(dep.clone());
        let mut facts = BTreeMap::new();
        facts.insert(
            root_id.clone(),
            PkgFacts {
                is_third_party: false,
                root_dir: Some(std::path::PathBuf::from("/repo/crates/aterm")),
                ..PkgFacts::default()
            },
        );
        facts.insert(
            dep.clone(),
            PkgFacts {
                loc: if dir.is_some() { 120 } else { 0 },
                is_third_party: true,
                root_dir: dir,
                ..PkgFacts::default()
            },
        );
        CellSurvey {
            cell: Cell {
                name: name.to_string(),
                triple: "aarch64-apple-darwin".to_string(),
                package: "aterm".to_string(),
            },
            graph,
            facts,
        }
    };

    let unmeasured = make_cell("missing-cell", &missing_id, None);
    let err = require_measured_sources(vec![unmeasured.clone()], Vec::new())
        .expect_err("sole cell with unmeasured third-party package must fail closed");
    assert!(err.contains("unpacked-dep@1.2.3"), "{err}");

    let measured = make_cell(
        "measured-cell",
        &present_id,
        Some(std::path::PathBuf::from("/repo/vendor/vendored-dep")),
    );
    let (kept, skipped) =
        require_measured_sources(vec![measured, unmeasured], Vec::new()).expect("one good cell");
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].cell.name, "measured-cell");
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].0, "missing-cell");
    assert!(skipped[0].1.contains("unpacked-dep@1.2.3"), "{skipped:?}");
}
