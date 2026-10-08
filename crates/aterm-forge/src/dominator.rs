// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! True cost: `dom(C) = reach(root) \ reach(root, block C)`.
//!
//! The set of packages that would leave the graph if `C` left it: `C` plus
//! everything only `C` was holding in. This is the only sanctioned answer to
//! "what does this dependency cost"; a subtree count double-bills shared
//! dependencies (e.g. a subtree of dozens whose dominator is a handful).
//!
//! The counterfactual blocks a node during traversal rather than mutating the
//! graph, so the same [`crate::model::Graph`] answers every query.

use crate::model::{CellSurvey, PkgId};
use std::collections::BTreeSet;

/// What falls if one package falls, third-party only: a first-party member
/// reachable only through third-party parents (a `[patch.crates-io]` target)
/// is not billed to the dependency. See [`dom_against`].
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

/// The dominator cost of one package in one cell. A package that is not in the
/// graph costs nothing, which is the honest answer: it is not there.
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
    // LOC descending, then name/version ascending, so the table is stable
    // across runs and two versions of one crate sort adjacently.
    out.sort_by(|a, b| b.1.loc.cmp(&a.1.loc).then_with(|| a.0.cmp(&b.0)));
    out
}

fn dom_against(s: &CellSurvey, base: &BTreeSet<PkgId>, target: &PkgId) -> DomCost {
    let without = s.graph.reach(Some(target));
    let mut cost = DomCost::default();
    // `BTreeSet::difference` yields ascending order, so `also` is sorted by
    // construction rather than by a second pass.
    for id in base.difference(&without) {
        // First-party removals are not a third-party cost: `[patch.crates-io]`
        // targets such as `crates/aterm-profiling` hang only under third-party
        // parents and would otherwise be billed to them (and break the survey's
        // partition check).
        //
        // A package with no `facts` entry is counted in: a measurement gap is
        // not evidence of first-partyness.
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
    use crate::resolve::default_cells;
    use std::collections::BTreeMap;
    fn survey(cell_index: usize) -> CellSurvey {
        let cells = default_cells();
        crate::loc::real_workspace_survey(&cells[cell_index])
            .unwrap_or_else(|e| panic!("cell must survey offline: {e}"))
    }

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
        // CONTROL: with it third-party (above), `b` costs 2 / 200.
        let b = dom(&s, &PkgId::new("b", "1.0.0"));
        assert_eq!(
            (b.pkgs, b.loc),
            (1, 100),
            "a first-party package that falls with `b` is not part of what `b` costs"
        );
        assert!(b.also.is_empty(), "nor is it listed as also-removed");
    }

    #[test]
    fn a_package_outside_the_graph_costs_nothing() {
        assert_eq!(
            dom(&synthetic(), &PkgId::new("absent", "0.1.0")),
            DomCost::default()
        );
    }

    #[test]
    fn ranked_is_third_party_only_and_sorted_by_loc_then_name() {
        let s = synthetic();
        let r = ranked(&s);
        assert_eq!(
            r.len(),
            4,
            "the root is not third-party and is never ranked"
        );
        assert!(r.iter().all(|(id, _)| id.name != "root"));
        assert!(r.windows(2).all(|w| w[0].1.loc >= w[1].1.loc));
        assert_eq!(r[0].0, PkgId::new("b", "1.0.0"), "b at 200 LOC leads");
        // The 100-LOC tail sorts by name: a, only_b, shared.
        let tail: Vec<&str> = r[1..].iter().map(|(id, _)| id.name.as_str()).collect();
        assert_eq!(tail, ["a", "only_b", "shared"]);
    }

    // ---- the real repository: regression constants ------------------------

    #[test]
    fn naga_falls_with_wgpu_and_libc_has_left_the_surface() {
        let s = survey(0);
        // wgpu and naga are absent from mac-arm (the Metal renderer is the
        // default; wgpu is a dev-edge oracle). Asserted as absence, since a
        // zero dominator would also be reported for an unseen package.
        for gone in ["wgpu", "wgpu-hal", "wgpu-core", "naga"] {
            assert!(
                !s.third_party().any(|id| id.name == gone),
                "`{gone}` must not be in the mac-arm normal graph post-flip \
                 (the oracle rides a dev edge, outside this survey by design)"
            );
        }

        // `libc` is first-party (`crates/aterm-libc`); asserted as absence,
        // since an empty dominator also holds for a package that is gone.
        assert!(
            !s.third_party().any(|id| id.name == "libc"),
            "libc is patched to crates/aterm-libc and must not be third-party"
        );

        // The objc2 family is absent from mac-arm (ported onto aterm-objc);
        // asserted as absence for the same reason.
        for gone in [
            "objc2",
            "objc2-app-kit",
            "objc2-foundation",
            "objc2-encode",
            "objc-sys",
            "block2",
            "dispatch",
        ] {
            assert!(
                !s.third_party().any(|id| id.name == gone),
                "`{gone}` must not be in the mac-arm normal graph after the objc2 exit \
                 (crates/aterm-objc/tests/objc2_exit_condition.rs holds both halves at zero)"
            );
        }
    }

    /// The shape every `DomCost` owes over the real mac-arm ranking: at least
    /// itself, at most the whole surface, `also` sorted without the target and
    /// one fewer than `pkgs`. Each third-party package ranks once.
    #[test]
    fn no_dominator_can_exceed_the_whole_third_party_surface() {
        let s = survey(0);
        let total = s.third_party_loc();
        let r = ranked(&s);
        assert_eq!(
            r.len(),
            s.third_party().count(),
            "every third-party package is ranked, none twice"
        );
        for (id, cost) in r {
            assert!(cost.pkgs >= 1, "{id} must at least cost itself");
            assert!(
                cost.loc <= total,
                "dom({id}) = {} exceeds the surface {total}",
                cost.loc
            );
            assert_eq!(
                cost.also.len(),
                cost.pkgs - 1,
                "dom({id}): `also` excludes the target itself"
            );
            assert!(!cost.also.contains(&id), "dom({id}) lists itself");
            assert!(
                cost.also.windows(2).all(|w| w[0] < w[1]),
                "dom({id}): `also` must be sorted"
            );
        }
    }
}
