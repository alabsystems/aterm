// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::model::{Cell, Graph, PkgFacts};
use std::collections::BTreeMap;

fn synthetic() -> CellSurvey {
    let ids: BTreeMap<&str, PkgId> = ["root", "a", "b", "shared", "only_b"]
        .into_iter()
        .map(|n| (n, PkgId::new(n, "1.0.0")))
        .collect();
    let mut graph = Graph {
        root: ids["root"].clone(),
        ..Graph::default()
    };
    for id in ids.values() {
        graph.nodes.insert(id.clone());
    }
    let mut edge = |from: &str, to: &str| {
        graph
            .edges
            .entry(ids[from].clone())
            .or_default()
            .insert(ids[to].clone());
    };
    edge("root", "a");
    edge("root", "b");
    edge("a", "shared");
    edge("b", "shared");
    edge("b", "only_b");
    let facts = ids
        .values()
        .map(|id| {
            let third = id.name != "root";
            (
                id.clone(),
                PkgFacts {
                    loc: 100,
                    is_third_party: third,
                    ..PkgFacts::default()
                },
            )
        })
        .collect();
    CellSurvey {
        cell: Cell {
            name: "synthetic".into(),
            triple: "x".into(),
            package: "root".into(),
        },
        graph,
        facts,
    }
}

#[test]
fn a_shared_dependency_is_not_billed_to_either_parent() {
    let s = synthetic();
    let a = dom(&s, &PkgId::new("a", "1.0.0"));
    assert_eq!((a.pkgs, a.loc), (1, 100));
    assert!(a.also.is_empty());
    let b = dom(&s, &PkgId::new("b", "1.0.0"));
    assert_eq!((b.pkgs, b.loc), (2, 200));
    assert_eq!(b.also, vec![PkgId::new("only_b", "1.0.0")]);
    assert_eq!(dom(&s, &PkgId::new("shared", "1.0.0")).pkgs, 1);
}

#[test]
fn blocking_the_root_costs_the_whole_third_party_surface() {
    let s = synthetic();
    let d = dom(&s, &s.graph.root);
    assert_eq!(d.pkgs, 4);
    assert_eq!(d.pkgs, s.third_party().count());
    assert_eq!(d.loc, s.third_party_loc());
    assert!(
        d.also.iter().all(|id| id.name != "root"),
        "the root is not a third-party package that falls with itself"
    );
}

#[test]
fn a_first_party_node_under_a_third_party_parent_is_not_billed_to_it() {
    let mut s = synthetic();
    let shim = PkgId::new("only_b", "1.0.0");
    s.facts
        .get_mut(&shim)
        .expect("fixture has only_b")
        .is_third_party = false;
    let b = dom(&s, &PkgId::new("b", "1.0.0"));
    assert_eq!(
        (b.pkgs, b.loc),
        (1, 100),
        "a first-party package that falls with `b` is not part of what `b` costs"
    );
    assert!(b.also.is_empty(), "nor is it listed as also-removed");
}
