// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! True cost: `dom(C) = reach(root) \ reach(root, block C)`.
//!
//! The set of packages that would leave the graph if `C` left it: `C` plus everything only `C`
//! was holding in. A subtree count double-bills shared dependencies.
//!
//! The counterfactual blocks a node during traversal rather than mutating the
//! graph, so the same [`crate::model::Graph`] answers every query.

use crate::model::{CellSurvey, PkgId};
use std::collections::BTreeSet;

/// What falls if one package falls, third-party only: a first-party member reachable only
/// through third-party parents (a `[patch.crates-io]` target) is not billed to the dependency.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DomCost {
    /// Third-party packages removed, including the target when it is
    /// third-party. A leaf nobody else needs costs 1, never 0.
    pub pkgs: usize,
    /// Physical `*.rs` lines summed over those packages.
    pub loc: u64,
    /// The other third-party packages that fall with it, sorted. Empty for a
    /// leaf.
    pub also: Vec<PkgId>,
}

/// The dominator cost of one package in one cell. A package not in the graph costs nothing.
pub fn dom(s: &CellSurvey, target: &PkgId) -> DomCost {
    dom_against(s, &s.graph.reach(None), target)
}

/// Every third-party package with its cost, heaviest first.
///
/// About N traversals of an N-node graph, deliberately: the graph is a few
/// hundred nodes, and an incremental dominator algorithm would be a second
/// definition of the number.
pub fn ranked(s: &CellSurvey) -> Vec<(PkgId, DomCost)> {
    let base = s.graph.reach(None);
    let mut out: Vec<(PkgId, DomCost)> = s
        .third_party()
        .map(|id| (id.clone(), dom_against(s, &base, id)))
        .collect();
    // LOC descending, then (name, version), so the table is stable across runs.
    out.sort_by(|a, b| b.1.loc.cmp(&a.1.loc).then_with(|| a.0.cmp(&b.0)));
    out
}

fn dom_against(s: &CellSurvey, base: &BTreeSet<PkgId>, target: &PkgId) -> DomCost {
    let without = s.graph.reach(Some(target));
    let mut cost = DomCost::default();
    // `difference` yields ascending order, so `also` is sorted.
    for id in base.difference(&without) {
        // First-party removals are not a third-party cost: `[patch.crates-io]` targets such as
        // `crates/aterm-profiling` hang only under third-party parents and would otherwise be
        // billed to them (breaking the survey's partition check). A package with no `facts`
        // entry counts: a measurement gap is not evidence of first-partyness.
        if s.facts.get(id).is_some_and(|f| !f.is_third_party) {
            continue;
        }
        cost.pkgs += 1;
        cost.loc += s.facts.get(id).map_or(0, |f| f.loc);
        if id != target {
            cost.also.push(id.clone());
        }
    }
    cost
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Cell, Graph, PkgFacts};
    use std::collections::BTreeMap;

    // ---- synthetic: the property that makes a dominator not a subtree -----

    fn synthetic() -> CellSurvey {
        // root -> a -> shared, root -> b -> shared, b -> only_b
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
        // `a`'s subtree is {a, shared} = 2, but dropping `a` frees only `a`:
        // `b` still holds `shared`.
        let a = dom(&s, &PkgId::new("a", "1.0.0"));
        assert_eq!((a.pkgs, a.loc), (1, 100));
        assert!(a.also.is_empty());
        // `b` frees itself and `only_b`, still not `shared`.
        let b = dom(&s, &PkgId::new("b", "1.0.0"));
        assert_eq!((b.pkgs, b.loc), (2, 200));
        assert_eq!(b.also, vec![PkgId::new("only_b", "1.0.0")]);
        // `shared` frees only itself, from either parent.
        assert_eq!(dom(&s, &PkgId::new("shared", "1.0.0")).pkgs, 1);
    }

    /// Blocking the root frees the whole third-party surface (4 of 5 nodes;
    /// the root is first-party). Asserted as `third_party().count()` so the
    /// two definitions of the surface cannot drift.
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

    /// A first-party node reached only through a third-party parent (the
    /// `[patch.crates-io]` shape) is not counted in the parent's dominator.
    #[test]
    fn a_first_party_node_under_a_third_party_parent_is_not_billed_to_it() {
        let mut s = synthetic();
        // `only_b` is reachable from `b` alone; make it first-party, the way a
        // patched-in replacement is.
        let shim = PkgId::new("only_b", "1.0.0");
        s.facts
            .get_mut(&shim)
            .expect("fixture has only_b")
            .is_third_party = false;
        // Control: with `only_b` third-party, `b` costs 2 / 200 (the test above).
        let b = dom(&s, &PkgId::new("b", "1.0.0"));
        assert_eq!(
            (b.pkgs, b.loc),
            (1, 100),
            "a first-party package that falls with `b` is not part of what `b` costs"
        );
        assert!(b.also.is_empty(), "nor is it listed as also-removed");
    }
}
