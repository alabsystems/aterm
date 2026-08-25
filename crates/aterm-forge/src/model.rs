// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The shared vocabulary: a package identity, a resolved graph, a target cell,
//! and the per-package facts every verb reports on.
//!
//! Two rules are encoded in these types rather than in prose, because both were
//! measured mistakes in the design phase:
//!
//!   1. A package is `(name, version)` and NEVER a bare name. `cargo pkgid -p
//!      winnow` errors "specification `winnow` is ambiguous" the moment a second
//!      major version enters the graph — and one already has (`winnow 0.7.15`
//!      forked in `vendor/`, `winnow 1.0.3` unpatched from the registry).
//!   2. Cost is a DOMINATOR, not a subtree. Measured: `softbuffer`'s subtree is
//!      39–45 packages in isolation and 8 in the real Linux graph, because winit
//!      already shares wayland-client and x11rb. [`crate::dominator`] is the only
//!      sanctioned way to answer "what does this edge cost".

use std::collections::{BTreeMap, BTreeSet};

/// A resolved package identity. Ordering is (name, version) so reports are
/// stable and two versions of one crate sort adjacently.
///
/// `Default` is derived only so [`Graph`] can derive it — an empty `PkgId` is
/// never a legal package and must never be reported as one.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PkgId {
    pub name: String,
    pub version: String,
}

impl PkgId {
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self { name: name.into(), version: version.into() }
    }

    /// The unambiguous cargo `-p` spelling. Never emit a bare name.
    pub fn spec(&self) -> String {
        format!("{}@{}", self.name, self.version)
    }
}

impl std::fmt::Display for PkgId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.name, self.version)
    }
}

/// One (triple, root package) measurement cell.
///
/// RESOLVE cells are mandatory and offline — resolution needs no toolchain
/// (measured: `cargo metadata --filter-platform x86_64-pc-windows-msvc` exits 0
/// on a host with no Windows std installed). BUILD cells are best-effort: a
/// missing std is an HONEST SKIP, counted and named, never a silent pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    /// Short handle used in reports and policy (`mac-arm`, `linux`, …).
    pub name: String,
    pub triple: String,
    /// The root package whose shipped graph this cell measures.
    pub package: String,
}

/// A resolved dependency graph for one cell: normal edges only, rooted at
/// [`Cell::package`].
#[derive(Clone, Debug, Default)]
pub struct Graph {
    pub root: PkgId,
    pub nodes: BTreeSet<PkgId>,
    pub edges: BTreeMap<PkgId, BTreeSet<PkgId>>,
}

impl Graph {
    /// Packages reachable from the root, optionally with one node blocked.
    /// Blocking is how [`crate::dominator`] computes a counterfactual without
    /// mutating anything.
    pub fn reach(&self, blocked: Option<&PkgId>) -> BTreeSet<PkgId> {
        let mut seen = BTreeSet::new();
        if Some(&self.root) == blocked {
            return seen;
        }
        seen.insert(self.root.clone());
        let mut stack = vec![self.root.clone()];
        while let Some(u) = stack.pop() {
            let Some(children) = self.edges.get(&u) else { continue };
            for v in children {
                if Some(v) == blocked || seen.contains(v) {
                    continue;
                }
                seen.insert(v.clone());
                stack.push(v.clone());
            }
        }
        seen
    }
}

/// Per-package facts, measured once and reused by every verb.
#[derive(Clone, Debug, Default)]
pub struct PkgFacts {
    /// Physical lines across every `*.rs` under the package root
    /// (`loc_method = "rs-physical-all-files-v1"`).
    pub loc: u64,
    /// `\bunsafe\b` TOKENS, not `unsafe {` blocks. The objc2 crates emit nearly
    /// all their unsafe from `extern_methods!`/`extern_class!`, so a block count
    /// reads 0 for an 83k-line crate.
    pub unsafe_tokens: u64,
    /// A build script is arbitrary code executed by the compiler, and
    /// `targo trust` marks every one `-Ztrust-verify=off` unconditionally. It is
    /// worth more to the opt-out than thousands of lines of leaf data code, so
    /// it is a first-class budget row rather than a footnote.
    pub has_build_rs: bool,
    pub is_proc_macro: bool,
    pub license: String,
    /// `true` when the package's manifest is NOT under `crates/`. The six
    /// `[patch.crates-io]` path packages count as third-party, which they are.
    pub is_third_party: bool,
    /// Absolute path to the package's manifest directory, when known.
    pub root_dir: Option<std::path::PathBuf>,
}

/// A whole cell's measurement: the graph plus the facts for every node in it.
#[derive(Clone, Debug)]
pub struct CellSurvey {
    pub cell: Cell,
    pub graph: Graph,
    pub facts: BTreeMap<PkgId, PkgFacts>,
}

impl CellSurvey {
    pub fn third_party(&self) -> impl Iterator<Item = &PkgId> {
        self.graph
            .nodes
            .iter()
            .filter(|p| self.facts.get(*p).is_some_and(|f| f.is_third_party))
    }

    pub fn third_party_loc(&self) -> u64 {
        self.third_party().filter_map(|p| self.facts.get(p)).map(|f| f.loc).sum()
    }

    pub fn build_scripts(&self) -> usize {
        self.third_party().filter(|p| self.facts.get(*p).is_some_and(|f| f.has_build_rs)).count()
    }

    pub fn proc_macros(&self) -> usize {
        self.third_party().filter(|p| self.facts.get(*p).is_some_and(|f| f.is_proc_macro)).count()
    }

    /// Names appearing at two or more versions in this cell — the dedup
    /// opportunity, reported separately from removals because collapsing a
    /// duplicate is NOT the same as deleting a dependency.
    pub fn duplicate_names(&self) -> BTreeMap<String, Vec<String>> {
        let mut by_name: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for p in self.third_party() {
            by_name.entry(p.name.clone()).or_default().push(p.version.clone());
        }
        by_name.retain(|_, v| v.len() > 1);
        by_name
    }
}
