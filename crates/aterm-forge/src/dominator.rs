// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! True cost: `dom(C) = reach(root) \ reach(root, block C)`.
//!
//! The set of packages that would leave the graph if `C` left it — `C` itself
//! plus everything only `C` was holding in. This is the ONLY sanctioned answer
//! to "what does this dependency cost", and the reason is measured: in the
//! Linux cell `softbuffer`'s subtree is 39–45 packages in isolation but its
//! dominator is 8, because winit already pulls wayland-client and x11rb. A
//! subtree count double-bills every shared dependency and would tell a carve
//! plan to delete packages that are not going anywhere.
//!
//! The counterfactual is computed by BLOCKING a node during traversal, never by
//! mutating the graph, so the same [`crate::model::Graph`] answers every query.

use crate::model::{CellSurvey, PkgId};
use std::collections::BTreeSet;

/// What falls if one package falls.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DomCost {
    /// Packages removed, INCLUDING the target. A leaf nobody else needs costs
    /// 1, never 0 — `dom(libc) = 1 / 127,772` is a real, quotable number.
    pub pkgs: usize,
    /// Physical `*.rs` lines summed over those packages.
    pub loc: u64,
    /// The other packages that fall with it, sorted. Empty for a leaf.
    pub also: Vec<PkgId>,
}

/// The dominator cost of one package in one cell. A package that is not in the
/// graph costs nothing, which is the honest answer: it is not there.
pub fn dom(s: &CellSurvey, target: &PkgId) -> DomCost {
    dom_against(s, &s.graph.reach(None), target)
}

/// Every third-party package with its cost, heaviest first.
///
/// This is ~N traversals of an N-node graph and it is deliberately not
/// cleverer than that: the graph is a few hundred nodes, the reference
/// implementation does the same thing in about a second, and an incremental
/// dominator algorithm would be a second definition of the number this whole
/// program exists to defend.
pub fn ranked(s: &CellSurvey) -> Vec<(PkgId, DomCost)> {
    let base = s.graph.reach(None);
    let mut out: Vec<(PkgId, DomCost)> =
        s.third_party().map(|id| (id.clone(), dom_against(s, &base, id))).collect();
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
    use crate::loc::survey_cell;
    use crate::model::{Cell, Graph, PkgFacts};
    use crate::resolve::default_cells;
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("crates/aterm-forge sits two levels under the workspace root")
            .to_path_buf()
    }

    fn survey(cell_index: usize) -> CellSurvey {
        let cells = default_cells();
        survey_cell(&repo_root(), &cells[cell_index])
            .unwrap_or_else(|e| panic!("cell must survey offline: {e}"))
    }

    fn find(s: &CellSurvey, name: &str) -> PkgId {
        let mut hits: Vec<&PkgId> = s.graph.nodes.iter().filter(|p| p.name == name).collect();
        assert_eq!(hits.len(), 1, "`{name}` must be unambiguous in this cell, got {hits:?}");
        hits.pop().expect("checked above").clone()
    }

    fn assert_dom(s: &CellSurvey, name: &str, pkgs: usize, loc: u64) {
        let id = find(s, name);
        let cost = dom(s, &id);
        assert_eq!((cost.pkgs, cost.loc), (pkgs, loc), "dom({id})");
        assert_eq!(cost.also.len(), pkgs - 1, "`also` excludes the target itself");
        assert!(!cost.also.contains(&id));
        assert!(cost.also.windows(2).all(|w| w[0] < w[1]), "`also` must be sorted");
    }

    // ---- synthetic: the property that makes a dominator not a subtree -----

    fn synthetic() -> CellSurvey {
        // root -> a -> shared, root -> b -> shared, b -> only_b
        let ids: BTreeMap<&str, PkgId> = ["root", "a", "b", "shared", "only_b"]
            .into_iter()
            .map(|n| (n, PkgId::new(n, "1.0.0")))
            .collect();
        let mut graph = Graph { root: ids["root"].clone(), ..Graph::default() };
        for id in ids.values() {
            graph.nodes.insert(id.clone());
        }
        let mut edge = |from: &str, to: &str| {
            graph.edges.entry(ids[from].clone()).or_default().insert(ids[to].clone());
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
                (id.clone(), PkgFacts { loc: 100, is_third_party: third, ..PkgFacts::default() })
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

    #[test]
    fn blocking_the_root_costs_the_whole_graph() {
        let s = synthetic();
        assert_eq!(dom(&s, &s.graph.root).pkgs, 5);
    }

    #[test]
    fn a_package_outside_the_graph_costs_nothing() {
        assert_eq!(dom(&synthetic(), &PkgId::new("absent", "0.1.0")), DomCost::default());
    }

    #[test]
    fn ranked_is_third_party_only_and_sorted_by_loc_then_name() {
        let s = synthetic();
        let r = ranked(&s);
        assert_eq!(r.len(), 4, "the root is not third-party and is never ranked");
        assert!(r.iter().all(|(id, _)| id.name != "root"));
        assert!(r.windows(2).all(|w| w[0].1.loc >= w[1].1.loc));
        assert_eq!(r[0].0, PkgId::new("b", "1.0.0"), "b at 200 LOC leads");
        // The 100-LOC tail sorts by name: a, only_b, shared.
        let tail: Vec<&str> = r[1..].iter().map(|(id, _)| id.name.as_str()).collect();
        assert_eq!(tail, ["a", "only_b", "shared"]);
    }

    // ---- the real repository: regression constants ------------------------

    #[test]
    fn mac_arm_dominator_costs_hold() {
        let s = survey(0);
        assert_dom(&s, "wgpu", 29, 398_302);
        assert_dom(&s, "naga", 8, 212_320);
        assert_dom(&s, "regex", 4, 158_471);
        assert_dom(&s, "libc", 1, 127_772);
        assert_dom(&s, "tracing", 3, 84_483);
    }

    #[test]
    fn naga_falls_with_wgpu_and_libc_falls_alone() {
        let s = survey(0);
        let wgpu = dom(&s, &find(&s, "wgpu"));
        assert!(wgpu.also.contains(&find(&s, "naga")), "naga is wgpu's alone");
        assert!(dom(&s, &find(&s, "libc")).also.is_empty(), "libc is a leaf");
    }

    /// DISCREPANCY, recorded rather than smoothed over. The design note records
    /// `dom(ureq@3.3.0) = 8 pkgs / 71,834 LOC`; this checkout measures 9 /
    /// 72,528. The whole difference is `percent-encoding 2.3.2` (694 LOC),
    /// whose ONLY parent in the mac-arm graph is ureq itself — so it must fall
    /// with ureq. This test pins both halves of that claim so the next person
    /// to re-measure sees the reason, not just a number that moved.
    #[test]
    fn ureq_costs_nine_packages_because_percent_encoding_has_no_other_parent() {
        let s = survey(0);
        let ureq = find(&s, "ureq");
        let pe = find(&s, "percent-encoding");
        let parents: Vec<&PkgId> = s
            .graph
            .edges
            .iter()
            .filter(|(_, kids)| kids.contains(&pe))
            .map(|(parent, _)| parent)
            .collect();
        assert_eq!(parents, vec![&ureq], "percent-encoding hangs off ureq alone");

        let cost = dom(&s, &ureq);
        assert_eq!((cost.pkgs, cost.loc), (9, 72_528));
        assert!(cost.also.contains(&pe));
        // Subtracting the one contested package reproduces the recorded figure
        // exactly, which is what makes this a bookkeeping delta and not a
        // different graph.
        assert_eq!(cost.pkgs - 1, 8);
        assert_eq!(cost.loc - s.facts[&pe].loc, 71_834);
    }

    #[test]
    fn linux_dominator_costs_hold() {
        let s = survey(1);
        assert_dom(&s, "accesskit_unix", 57, 241_084);
        assert_dom(&s, "sctk-adwaita", 7, 31_776);
        assert_dom(&s, "accesskit_winit", 58, 242_598);
        // accesskit_winit is accesskit_unix plus exactly one more package.
        let unix = dom(&s, &find(&s, "accesskit_unix"));
        let winit = dom(&s, &find(&s, "accesskit_winit"));
        assert_eq!(winit.pkgs - unix.pkgs, 1);
    }

    #[test]
    fn the_mac_arm_ranking_leads_with_wgpu_naga_regex_libc_tracing() {
        let r = ranked(&survey(0));
        let head: Vec<(&str, usize, u64)> =
            r.iter().take(5).map(|(id, c)| (id.name.as_str(), c.pkgs, c.loc)).collect();
        assert_eq!(
            head,
            [
                ("wgpu", 29, 398_302),
                ("naga", 8, 212_320),
                ("regex", 4, 158_471),
                ("libc", 1, 127_772),
                ("tracing", 3, 84_483),
            ]
        );
        assert_eq!(r.len(), 153, "every third-party package is ranked, none twice");
    }

    #[test]
    fn no_dominator_can_exceed_the_whole_third_party_surface() {
        let s = survey(0);
        let total = s.third_party_loc();
        for (id, cost) in ranked(&s) {
            assert!(cost.pkgs >= 1, "{id} must at least cost itself");
            assert!(cost.loc <= total, "dom({id}) = {} exceeds the surface {total}", cost.loc);
        }
    }
}
