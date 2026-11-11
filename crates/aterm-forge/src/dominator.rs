// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Dominator cost: `dom(C) = reach(root) \ reach(root, block C)`.

use crate::model::{CellSurvey, PkgId};
use std::collections::BTreeSet;

/// Third-party packages and lines that fall when one package is removed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DomCost {
    /// Third-party packages removed, including the target when it is third-party.
    pub pkgs: usize,
    /// Physical `*.rs` lines summed over those packages.
    pub loc: u64,
    /// The other third-party packages that fall with it, sorted.
    pub also: Vec<PkgId>,
}

/// The dominator cost of one package in one cell.
pub fn dom(s: &CellSurvey, target: &PkgId) -> DomCost {
    dom_against(s, &s.graph.reach(None), target)
}

/// Every third-party package with its cost, heaviest first.
pub fn ranked(s: &CellSurvey) -> Vec<(PkgId, DomCost)> {
    let base = s.graph.reach(None);
    let mut out: Vec<(PkgId, DomCost)> = s
        .third_party()
        .map(|id| (id.clone(), dom_against(s, &base, id)))
        .collect();
    out.sort_by(|a, b| b.1.loc.cmp(&a.1.loc).then_with(|| a.0.cmp(&b.0)));
    out
}

fn dom_against(s: &CellSurvey, base: &BTreeSet<PkgId>, target: &PkgId) -> DomCost {
    let without = s.graph.reach(Some(target));
    let mut cost = DomCost::default();
    for id in base.difference(&without) {
        let Some(facts) = s.facts.get(id).filter(|f| f.is_third_party) else {
            continue;
        };
        cost.pkgs += 1;
        cost.loc += facts.loc;
        if id != target {
            cost.also.push(id.clone());
        }
    }
    cost
}

#[cfg(test)]
#[path = "dominator_tests.rs"]
mod tests;
