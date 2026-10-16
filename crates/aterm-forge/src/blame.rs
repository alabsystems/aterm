// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `targo --unverified forge blame`: why is this package here?
//!
//! Answers three questions a `cargo tree | grep` cannot:
//!
//!   1. **Which first-party line is responsible.** Every path from the cell root
//!      to the package is enumerated (the shortest printed, the rest counted),
//!      with the direct dependants: the manifest lines that would have to
//!      change.
//!   2. **What it costs.** The dominator cost per cell, not the subtree.
//!   3. **Whether the fork is live.** A `[patch.crates-io]` entry replaces one
//!      version. A dependant asking for a different major gets the unpatched
//!      registry copy in the same build, and cargo says nothing; this verb
//!      reports it.
//!
//! A bare `name` is never refused for ambiguity (as `cargo pkgid` does); every
//! matching version is listed with the cells that hold it.

use crate::model::{CellSurvey, Graph, PkgId};
use crate::survey::{commas, fit, fit_path, survey_cells, wrap};
use crate::{Outcome, dominator, resolve};
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};
use std::fmt::Write as _;
use std::path::Path;

/// Paths printed per cell before the rest are counted rather than listed.
const PATHS_SHOWN: usize = 10;
/// Hard bound on the best-first path search, so a wide graph cannot hang the
/// report. When it trips the report SAYS so rather than quietly printing fewer.
const SEARCH_BUDGET: usize = 40_000;
const W_LINE: usize = 96;

/// What `[patch.crates-io]` actually did to this exact `(name, version)`.
#[derive(Clone, Debug, PartialEq, Eq)]
enum PatchState {
    /// This version IS the vendored fork aterm maintains.
    Fork { path: String },
    /// A fork of this NAME exists at another version, and this copy is the
    /// pristine registry code. The fork's fixes do not reach it.
    UnpatchedBesideFork { path: String, fork_version: String },
    /// A fork directory exists but its version could not be read.
    ForkVersionUnreadable { path: String },
    /// Ordinary registry code, no fork of this name.
    Registry,
    /// aterm's own code, vendored in-tree and reached by a path dependency
    /// (`provenance::FIRST_PARTY_VENDORED`). Neither a fork nor registry code.
    FirstPartyVendored { path: String },
}

#[derive(Clone, Debug, Default)]
struct VendorFork {
    path: String,
    version: Option<String>,
    /// The row is renamed, so it claims one version line of its package
    /// ([`crate::policy::in_row_scope`]).
    renamed: bool,
}

/// Package → the patch rows replacing it: one, or one per version line when
/// rows are renamed ([`crate::policy::patched_package`]).
type Forks = BTreeMap<String, Vec<VendorFork>>;

/// Explain one package. `pkg` is `name` or `name@version`.
pub fn run(root: &Path, pkg: &str, cells: &[String]) -> Result<Outcome, String> {
    let (want_name, want_version) = match pkg.split_once('@') {
        Some((n, v)) if !n.is_empty() && !v.is_empty() => (n.to_string(), Some(v.to_string())),
        Some(_) => {
            return Err(format!(
                "`{pkg}` is not a package spec — Fix: pass `name` or `name@version`, e.g. \
                 `targo --unverified forge blame indexmap` or `targo --unverified forge blame indexmap@2.14.0`."
            ));
        }
        None => (pkg.to_string(), None),
    };

    let all = resolve::default_cells();
    let chosen = resolve::select(&all, cells)?;
    let forks = read_forks(root)?;
    let (surveys, skipped) = survey_cells(root, &chosen).map_err(|why| {
        format!(
            "not one requested cell resolved, so `{pkg}` cannot be located:\n{why}    \
             Fix: run `targo fetch --locked` once in {}, then re-run.",
            root.display()
        )
    })?;

    // (name, version) -> the surveys that resolve it.
    let mut found: BTreeMap<PkgId, Vec<usize>> = BTreeMap::new();
    for (i, s) in surveys.iter().enumerate() {
        for id in &s.graph.nodes {
            if id.name == want_name && want_version.as_ref().is_none_or(|v| *v == id.version) {
                found.entry(id.clone()).or_default().push(i);
            }
        }
    }

    let mut log = String::new();
    let _ = writeln!(log, "{}", "=".repeat(W_LINE));
    let _ = writeln!(log, "FORGE BLAME — {pkg}");
    let _ = writeln!(log, "{}", "=".repeat(W_LINE));
    let names: Vec<&str> = surveys.iter().map(|s| s.cell.name.as_str()).collect();
    let _ = writeln!(log, "  cells searched   {}", names.join(", "));
    for (c, e) in &skipped {
        let _ = writeln!(log, "  CELL SKIPPED     {c}: {e}");
    }

    if found.is_empty() {
        not_found(
            &mut log,
            root,
            &want_name,
            want_version.as_deref(),
            &surveys,
        );
        return Ok(Outcome { ok: false, log });
    }

    if want_version.is_none() && found.len() > 1 {
        let _ = writeln!(
            log,
            "  spec             bare name, {} versions resolved — ALL are shown. (`cargo \
             pkgid\n                   -p {}` refuses this as ambiguous; the ambiguity is \
             the finding.)",
            found.len(),
            want_name
        );
    }

    versions_block(&mut log, root, &found, &surveys, &forks);
    liveness_block(&mut log, root, &want_name, &found, &surveys, &forks);
    for (id, cells_with) in &found {
        package_block(&mut log, root, id, cells_with, &surveys, &forks);
    }

    Ok(Outcome { ok: true, log })
}

// ------------------------------------------------------------------- sections

fn versions_block(
    log: &mut String,
    root: &Path,
    found: &BTreeMap<PkgId, Vec<usize>>,
    surveys: &[CellSurvey],
    forks: &Forks,
) {
    let _ = writeln!(log, "\n  VERSIONS RESOLVED");
    for (id, cells_with) in found {
        let where_ = cells_with
            .iter()
            .map(|i| surveys[*i].cell.name.as_str())
            .collect::<Vec<_>>();
        let facts = cells_with.iter().find_map(|i| surveys[*i].facts.get(id));
        let state = patch_state(root, facts, forks, id);
        let tag = match &state {
            PatchState::Fork { path } => format!("FORKED   {path}"),
            PatchState::UnpatchedBesideFork { .. } => "UNPATCHED (registry)".to_string(),
            PatchState::ForkVersionUnreadable { path } => format!("fork? {path}"),
            PatchState::Registry => "registry".to_string(),
            PatchState::FirstPartyVendored { .. } => "first-party (vendored)".to_string(),
        };
        let line = versions_row(&id.name, &id.version.to_string(), &tag, &where_.join(", "));
        let _ = writeln!(log, "{line}");
    }
    if found.len() > 1 {
        // The same arithmetic as `survey`'s duplicate-versions section, for
        // the one name asked about.
        let mut locs: Vec<u64> = found
            .keys()
            .map(|id| {
                surveys
                    .iter()
                    .find_map(|s| s.facts.get(id))
                    .map_or(0, |f| f.loc)
            })
            .collect();
        locs.sort_unstable();
        let prize: u64 = locs.iter().rev().skip(1).sum();
        wrap(
            log,
            "    ",
            &format!(
                "Collapsing these {} onto one version would retire {} LOC — every copy but \
                 the largest. That is a DEDUP (move a dependant's requirement), not a removal \
                 (delete an edge), and the two must not be added together.",
                found.len(),
                commas(prize)
            ),
            W_LINE,
        );
    }
}

/// THE load-bearing check: a fork that only covers one of the resolved versions.
fn liveness_block(
    log: &mut String,
    root: &Path,
    name: &str,
    found: &BTreeMap<PkgId, Vec<usize>>,
    surveys: &[CellSurvey],
    forks: &Forks,
) {
    let stale: Vec<(&PkgId, String, String, &Vec<usize>)> = found
        .iter()
        .filter_map(
            |(id, cells_with)| match patch_state(root, None, forks, id) {
                PatchState::UnpatchedBesideFork { path, fork_version } => {
                    Some((id, path, fork_version, cells_with))
                }
                _ => None,
            },
        )
        .collect();
    if stale.is_empty() {
        return;
    }

    let _ = writeln!(log, "\n{}", "!".repeat(W_LINE));
    let _ = writeln!(
        log,
        "PATCH LIVENESS DEFECT — the fork does not cover every resolved version"
    );
    let _ = writeln!(log, "{}", "!".repeat(W_LINE));
    for (id, fork_path, fork_version, cells_with) in &stale {
        let where_: Vec<&str> = cells_with
            .iter()
            .map(|i| surveys[*i].cell.name.as_str())
            .collect();
        wrap(
            log,
            "  ",
            &format!(
                "`{name}` is forked at {fork_version} under {path}, but {} {} ALSO resolves — \
                 pristine from the registry — in cell(s) {}. `[patch.crates-io]` substitutes ONE \
                 version: a dependant whose requirement the vendored manifest cannot satisfy \
                 silently gets upstream code, and every fix in the fork is absent from that copy.",
                id.name,
                id.version,
                where_.join(", "),
                path = fork_path
            ),
            W_LINE,
        );
        for i in *cells_with {
            let s = &surveys[*i];
            // A copy reached through a proc macro is compiled into the compiler's
            // own process — the strongest form of this defect.
            let mut via: Vec<PkgId> = ancestors(&s.graph, id)
                .into_iter()
                .filter(|a| s.facts.get(a).is_some_and(|f| f.is_proc_macro))
                .collect();
            via.sort();
            if !via.is_empty() {
                let shown: Vec<String> = via
                    .iter()
                    .take(3)
                    .map(|p| format!("{} {}", p.name, p.version))
                    .collect();
                let more = if via.len() > 3 {
                    format!(" (+{} more)", via.len() - 3)
                } else {
                    String::new()
                };
                wrap(
                    log,
                    "  ",
                    &format!(
                        "In cell {}, it is reached through the proc macro(s) {}{} — so the \
                         UNPATCHED copy is compiled and EXECUTED inside the compiler on every \
                         build of that cell.",
                        s.cell.name,
                        shown.join(", "),
                        more
                    ),
                    W_LINE,
                );
            }
            let (paths, _) = shortest_paths(&s.graph, id, 1);
            if let Some(path) = paths.first() {
                let _ = writeln!(
                    log,
                    "  Shortest path to the unpatched copy in {}:",
                    s.cell.name
                );
                write_path(log, "      ", path);
            }
        }
    }
    let _ = writeln!(
        log,
        "  Fix, in preference order:\n    \
         1. cut the edge — `targo --unverified forge blame` above names the first-party dependant; drop\n       \
            the feature or dependency that drags the second major in;\n    \
         2. move the fork forward — re-fork `{name}` at the other major under vendor/ so\n       \
            patched copy satisfies both requirements, then re-run `targo --unverified forge attest`;\n    \
         3. record it — add the exemption, with the reason and a re-review date, to\n       \
            vendor/forge.toml, which `targo --unverified forge check` reads.\n  \
         `targo --unverified forge check` is the gate that FAILS on this; blame only reports it."
    );
}

fn package_block(
    log: &mut String,
    root: &Path,
    id: &PkgId,
    cells_with: &[usize],
    surveys: &[CellSurvey],
    forks: &Forks,
) {
    let _ = writeln!(log, "\n{}", "-".repeat(W_LINE));
    let _ = writeln!(log, "{} {}", id.name, id.version);
    let _ = writeln!(log, "{}", "-".repeat(W_LINE));

    let facts = cells_with.iter().find_map(|i| surveys[*i].facts.get(id));
    let state = patch_state(root, facts, forks, id);
    match &state {
        PatchState::Fork { path } => {
            let _ = writeln!(
                log,
                "  source        {path} — PATCHED path package. aterm OWNS and maintains this copy."
            );
        }
        PatchState::UnpatchedBesideFork { path, fork_version } => {
            let _ = writeln!(
                log,
                "  source        crates.io registry — UNPATCHED, while {path} carries the fork at"
            );
            let _ = writeln!(
                log,
                "                {fork_version}. See the DEFECT block above."
            );
        }
        PatchState::ForkVersionUnreadable { path } => {
            let _ = writeln!(
                log,
                "  source        {path} exists but its manifest version could not be read — Fix:"
            );
            let _ = writeln!(
                log,
                "                give {path}/Cargo.toml a literal `version = \"…\"`."
            );
        }
        PatchState::Registry => {
            let _ = writeln!(
                log,
                "  source        crates.io registry (not forked, not owned)"
            );
        }
        PatchState::FirstPartyVendored { path } => {
            // Three lines: `vendor/<root>/crates/<name>` plus its sentence does
            // not fit in 100 columns.
            let _ = writeln!(
                log,
                "  source        {path} — FIRST-PARTY, vendored in-tree."
            );
            let _ = writeln!(
                log,
                "                aterm WROTE this code; it is under vendor/ so a clean clone"
            );
            let _ = writeln!(
                log,
                "                builds, not because it is a redistribution."
            );
        }
    }
    if let Some(f) = facts {
        let lic = if f.license.is_empty() {
            "license unknown"
        } else {
            f.license.as_str()
        };
        let _ = writeln!(
            log,
            "  facts         {} LOC   {} unsafe tokens   build.rs {}   proc-macro {}   {}",
            commas(f.loc),
            commas(f.unsafe_tokens),
            yes_no(f.has_build_rs),
            yes_no(f.is_proc_macro),
            lic
        );
        let _ = writeln!(
            log,
            "                {}",
            if f.is_third_party {
                "THIRD-PARTY (aterm did not write it)"
            } else {
                "first-party (aterm's own code)"
            }
        );
        if let Some(dir) = &f.root_dir {
            // In-repo dirs print repo-relative (the checkout prefix varies per
            // machine); out-of-repo registry dirs stay absolute but bounded,
            // eliding the constant prefix and keeping the package's own tail.
            let _ = writeln!(log, "                {}", dir_line(root, dir));
        }
        // If the facts were not measured from the fork's directory, say so and
        // give the fork's own line count and delta against what was measured.
        if let PatchState::Fork { path } = &state {
            let vendored = root.join(path);
            let measured_here = f
                .root_dir
                .as_ref()
                .is_some_and(|d| d.starts_with(&vendored));
            if !measured_here {
                let mine = rs_lines(&vendored);
                let delta = mine as i64 - f.loc as i64;
                wrap(
                    log,
                    "                ",
                    &format!(
                        "NOTE: those facts are the PRISTINE registry copy of {} {}, which \
                         `loc::package_dir` prefers by design so the ledger cannot move while \
                         the fork is being edited. The fork itself ({}) is {} lines, {}{} \
                         against upstream; that drift is `targo --unverified forge attest`'s business.",
                        id.name,
                        id.version,
                        path,
                        commas(mine),
                        if delta < 0 { "-" } else { "+" },
                        commas(delta.unsigned_abs())
                    ),
                    W_LINE,
                );
            }
        }
    } else {
        let _ = writeln!(
            log,
            "  facts         not measured — the fact table has no row for it."
        );
    }

    for i in cells_with {
        let s = &surveys[*i];
        let cost = dominator::dom(s, id);
        let (paths, budget_hit) = shortest_paths(&s.graph, id, PATHS_SHOWN);
        let total = count_paths(&s.graph, id);
        let _ = writeln!(
            log,
            "\n  cell {}   dom {} package(s) / {} LOC   {} path(s) from `{}`",
            s.cell.name,
            commas(cost.pkgs as u64),
            commas(cost.loc),
            if total.1 {
                format!(">{}", commas(total.0))
            } else {
                commas(total.0)
            },
            s.graph.root.name
        );
        if cost.pkgs > 1 {
            let mut also: Vec<String> = cost
                .also
                .iter()
                .filter(|p| *p != id)
                .map(|p| format!("{} {}", p.name, p.version))
                .collect();
            also.sort();
            let _ = writeln!(
                log,
                "      removing it also removes: {}",
                fit(&also.join(", "), 68)
            );
        }

        let entries = entry_points(s, id);
        if !entries.is_empty() {
            let _ = writeln!(
                log,
                "      first-party edges responsible: {}",
                fit(
                    &entries
                        .iter()
                        .map(|p| p.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    62
                )
            );
        }
        let direct = direct_dependants(&s.graph, id);
        if !direct.is_empty() {
            let mut d: Vec<String> = direct
                .iter()
                .map(|p| format!("{} {}", p.name, p.version))
                .collect();
            d.sort();
            let _ = writeln!(
                log,
                "      required directly by: {}",
                fit(&d.join(", "), 68)
            );
        }
        for (n, p) in paths.iter().enumerate() {
            write_path(log, &format!("      [{:>2}] ", n + 1), p);
        }
        if total.0 as usize > paths.len() {
            let _ = writeln!(
                log,
                "      … {} further path(s) not shown{}",
                commas(total.0 - paths.len() as u64),
                if budget_hit {
                    " (the path search hit its bound)"
                } else {
                    ""
                }
            );
        }
    }
}

fn not_found(
    log: &mut String,
    root: &Path,
    name: &str,
    version: Option<&str>,
    surveys: &[CellSurvey],
) {
    let spec = match version {
        Some(v) => format!("{name}@{v}"),
        None => name.to_string(),
    };
    let _ = writeln!(
        log,
        "\n  NOT RESOLVED — `{spec}` is in no surveyed cell's shipped graph."
    );
    for s in surveys {
        let _ = writeln!(
            log,
            "      {:<10} {} packages resolved ({} third-party)",
            s.cell.name,
            commas(s.graph.nodes.len() as u64),
            commas(s.third_party().count() as u64)
        );
    }

    // Same name, other version? That is a different (and useful) answer.
    if let Some(v) = version {
        let others: BTreeSet<&str> = surveys
            .iter()
            .flat_map(|s| s.graph.nodes.iter())
            .filter(|p| p.name == name)
            .map(|p| p.version.as_str())
            .collect();
        if !others.is_empty() {
            let _ = writeln!(
                log,
                "  `{name}` IS resolved, but not at {v} — resolved version(s): {}.\n  \
                 Fix: `targo --unverified forge blame {name}` (bare name) shows every version.",
                others.into_iter().collect::<Vec<_>>().join(", ")
            );
            return;
        }
    }

    let locked = lock_versions(root, name);
    if !locked.is_empty() {
        let _ = writeln!(
            log,
            "  It IS in Cargo.lock at {} — so it is reached only by dev/build edges, or by a\n  \
             workspace member that is not the cell's root package. The survey walks NORMAL\n  \
             edges from the shipped root by design.\n  \
             Fix: `targo tree -p aterm -e all -i {name}` to see the dev/build edge that keeps it.",
            locked.join(", "),
            name = name
        );
        return;
    }

    let near = nearby(name, surveys);
    if near.is_empty() {
        let _ = writeln!(log, "  No similarly-named package is resolved either.");
    } else {
        let _ = writeln!(log, "  Did you mean: {}?", near.join(", "));
    }
    let _ = writeln!(
        log,
        "  Fix: `targo --unverified forge survey --top 0` lists every resolved package with its exact\n  \
         version; `blame` takes `name` or `name@version` copied from there."
    );
}

// ------------------------------------------------------------------ graph work

fn parents_of(g: &Graph) -> BTreeMap<PkgId, BTreeSet<PkgId>> {
    let mut rev: BTreeMap<PkgId, BTreeSet<PkgId>> = BTreeMap::new();
    for (u, children) in &g.edges {
        for v in children {
            rev.entry(v.clone()).or_default().insert(u.clone());
        }
    }
    rev
}

/// Shortest hop count from the root to every reachable node (BFS).
fn depths(g: &Graph) -> BTreeMap<PkgId, usize> {
    let mut d: BTreeMap<PkgId, usize> = BTreeMap::new();
    d.insert(g.root.clone(), 0);
    let mut queue = std::collections::VecDeque::from([g.root.clone()]);
    while let Some(u) = queue.pop_front() {
        let du = d[&u];
        let Some(children) = g.edges.get(&u) else {
            continue;
        };
        for v in children {
            if !d.contains_key(v) {
                d.insert(v.clone(), du + 1);
                queue.push_back(v.clone());
            }
        }
    }
    d
}

/// Exact number of distinct root→target paths, memoised over the reverse graph.
/// Returns `(count, saturated)`; the `active` set makes a cycle degrade to an
/// under-count instead of a hang.
fn count_paths(g: &Graph, target: &PkgId) -> (u64, bool) {
    let rev = parents_of(g);
    let mut memo: BTreeMap<PkgId, u64> = BTreeMap::new();
    let mut active: BTreeSet<PkgId> = BTreeSet::new();
    let mut saturated = false;
    let n = walk(
        target,
        &g.root,
        &rev,
        &mut memo,
        &mut active,
        &mut saturated,
    );
    (n, saturated)
}

fn walk(
    node: &PkgId,
    root: &PkgId,
    rev: &BTreeMap<PkgId, BTreeSet<PkgId>>,
    memo: &mut BTreeMap<PkgId, u64>,
    active: &mut BTreeSet<PkgId>,
    saturated: &mut bool,
) -> u64 {
    if node == root {
        return 1;
    }
    if let Some(v) = memo.get(node) {
        return *v;
    }
    if !active.insert(node.clone()) {
        return 0; // cycle guard; a normal-edge graph has none
    }
    let mut total = 0u64;
    if let Some(ps) = rev.get(node) {
        for p in ps {
            let sub = walk(p, root, rev, memo, active, saturated);
            let (sum, over) = total.overflowing_add(sub);
            if over {
                *saturated = true;
                total = u64::MAX;
            } else {
                total = sum;
            }
        }
    }
    active.remove(node);
    memo.insert(node.clone(), total);
    total
}

/// The `want` shortest root→target paths, in nondecreasing length.
///
/// Best-first over the reverse graph with `f = depth(head) + hops(head→target)`,
/// an admissible and consistent heuristic, so the first `want` completions are
/// exactly the shortest. Searching backwards keeps the frontier inside the
/// target's ancestor set.
fn shortest_paths(g: &Graph, target: &PkgId, want: usize) -> (Vec<Vec<PkgId>>, bool) {
    let mut out: Vec<Vec<PkgId>> = Vec::new();
    if want == 0 || !g.nodes.contains(target) {
        return (out, false);
    }
    let d = depths(g);
    let Some(dt) = d.get(target) else {
        return (out, false);
    };
    let rev = parents_of(g);

    let mut heap: BinaryHeap<Reverse<(usize, Vec<PkgId>)>> = BinaryHeap::new();
    heap.push(Reverse((*dt, vec![target.clone()])));
    let mut pops = 0usize;
    while let Some(Reverse((_, suffix))) = heap.pop() {
        pops += 1;
        if pops > SEARCH_BUDGET {
            return (out, true);
        }
        let head = &suffix[0];
        if *head == g.root {
            out.push(suffix);
            if out.len() >= want {
                return (out, false);
            }
            continue;
        }
        let Some(ps) = rev.get(head) else { continue };
        for p in ps {
            let Some(dp) = d.get(p) else { continue };
            if suffix.contains(p) {
                continue;
            }
            let mut next = Vec::with_capacity(suffix.len() + 1);
            next.push(p.clone());
            next.extend(suffix.iter().cloned());
            heap.push(Reverse((dp + next.len() - 1, next)));
        }
    }
    (out, false)
}

/// Every node from which `target` is reachable.
fn ancestors(g: &Graph, target: &PkgId) -> BTreeSet<PkgId> {
    let rev = parents_of(g);
    let mut seen = BTreeSet::new();
    let mut stack = vec![target.clone()];
    while let Some(u) = stack.pop() {
        let Some(ps) = rev.get(&u) else { continue };
        for p in ps {
            if seen.insert(p.clone()) {
                stack.push(p.clone());
            }
        }
    }
    seen
}

fn direct_dependants(g: &Graph, target: &PkgId) -> BTreeSet<PkgId> {
    g.edges
        .iter()
        .filter(|(_, cs)| cs.contains(target))
        .map(|(u, _)| u.clone())
        .collect()
}

/// The root's direct dependencies from which the target is still reachable —
/// the first-party manifest lines that own this package's presence.
fn entry_points(s: &CellSurvey, target: &PkgId) -> Vec<PkgId> {
    let anc = ancestors(&s.graph, target);
    let Some(children) = s.graph.edges.get(&s.graph.root) else {
        return Vec::new();
    };
    children
        .iter()
        .filter(|c| *c == target || anc.contains(*c))
        .cloned()
        .collect()
}

// ------------------------------------------------------------------ vendor/fork

/// `[patch.crates-io]` entries with the vendored manifest's own version.
fn read_forks(root: &Path) -> Result<Forks, String> {
    let manifest = root.join("Cargo.toml");
    let text = std::fs::read_to_string(&manifest).map_err(|e| {
        format!(
            "cannot read {}: {e} — Fix: run `targo --unverified forge` from inside the workspace, or pass \
             `--root <workspace>`.",
            manifest.display()
        )
    })?;
    let doc = text.parse::<aterm_toml::edit::DocumentMut>().map_err(|e| {
        format!(
            "{} is not valid TOML: {e} — Fix: repair the manifest first.",
            manifest.display()
        )
    })?;
    let mut out = Forks::new();
    let Some(table) = doc
        .get("patch")
        .and_then(|p| p.get("crates-io"))
        .and_then(|c| c.as_table_like())
    else {
        return Ok(out);
    };
    for (key, item) in table.iter() {
        let Some(rel) = item.get("path").and_then(|p| p.as_str()) else {
            continue;
        };
        let name = crate::policy::patched_package(key, item);
        let version = std::fs::read_to_string(root.join(rel).join("Cargo.toml"))
            .ok()
            .and_then(|t| t.parse::<aterm_toml::edit::DocumentMut>().ok())
            .and_then(|d| {
                d.get("package")
                    .and_then(|p| p.get("version"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            });
        out.entry(name.to_string()).or_default().push(VendorFork {
            path: rel.to_string(),
            version,
            renamed: name != key,
        });
    }
    Ok(out)
}

fn patch_state(
    root: &Path,
    facts: Option<&crate::model::PkgFacts>,
    forks: &Forks,
    id: &PkgId,
) -> PatchState {
    // The roster is asked first, off the measured directory: a first-party
    // vendored crate has no patch entry and would otherwise read as registry
    // code.
    if let Some(dir) = facts.and_then(|f| f.root_dir.as_ref())
        && crate::provenance::is_first_party_vendored(root, dir)
    {
        return PatchState::FirstPartyVendored {
            path: dir_line(root, dir),
        };
    }
    let Some(rows) = forks.get(&id.name) else {
        return PatchState::Registry;
    };
    if let Some(f) = rows
        .iter()
        .find(|f| f.version.as_deref() == Some(id.version.as_str()))
    {
        return PatchState::Fork {
            path: f.path.clone(),
        };
    }
    // Not any row's own copy. Unpatched beside the first row that claims this
    // version; a version line no renamed row claims is ordinary registry code.
    let claimed = rows
        .iter()
        .find(|f| crate::policy::in_row_scope(f.renamed, f.version.as_deref(), &id.version));
    match claimed {
        Some(VendorFork {
            path,
            version: Some(v),
            ..
        }) => PatchState::UnpatchedBesideFork {
            path: path.clone(),
            fork_version: v.clone(),
        },
        Some(VendorFork { path, .. }) => PatchState::ForkVersionUnreadable { path: path.clone() },
        None => PatchState::Registry,
    }
}

/// Versions of `name` in `Cargo.lock`. Used only to explain an absence: a
/// package in the lock but not in the shipped graph is a dev/build edge.
fn lock_versions(root: &Path, name: &str) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(root.join("Cargo.lock")) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut hit = false;
    for line in text.lines() {
        let line = line.trim();
        if let Some(v) = line
            .strip_prefix("name = \"")
            .and_then(|r| r.strip_suffix('"'))
        {
            hit = v == name;
        } else if hit
            && let Some(v) = line
                .strip_prefix("version = \"")
                .and_then(|r| r.strip_suffix('"'))
        {
            out.push(v.to_string());
            hit = false;
        }
    }
    out
}

/// Cheap "did you mean": substring either way, then a shared prefix. No edit
/// distance — a suggestion list is a courtesy, not a search engine.
fn nearby(name: &str, surveys: &[CellSurvey]) -> Vec<String> {
    let needle = name.to_ascii_lowercase();
    let mut hits: BTreeSet<String> = BTreeSet::new();
    for s in surveys {
        for p in &s.graph.nodes {
            let cand = p.name.to_ascii_lowercase();
            let shared = cand
                .chars()
                .zip(needle.chars())
                .take_while(|(a, b)| a == b)
                .count();
            if cand.contains(&needle) || needle.contains(&cand) || shared >= 3 {
                hits.insert(p.name.clone());
            }
        }
    }
    hits.into_iter().take(8).collect()
}

// ------------------------------------------------------------------ formatting

/// The measured package's directory as the report shows it: repo-relative when
/// in the checkout, and bounded either way by [`fit_path`], keeping the package
/// directory the line names. The line's width depends on content, not on the
/// checkout location or `$CARGO_HOME` depth.
fn dir_line(root: &Path, dir: &Path) -> String {
    let shown = dir.strip_prefix(root).unwrap_or(dir);
    fit_path(&shown.display().to_string(), W_LINE - 16)
}

/// One versions-resolved row: `name version`, the patch tag, then the cells.
///
/// Each field is fitted one column short of its width, so a long name+version
/// never runs into the next field without a separating space.
fn versions_row(name: &str, version: &str, tag: &str, cells: &str) -> String {
    // The name yields and the version survives: the version is what this
    // section reports.
    const NAME_VERSION: usize = 27;
    let for_name = NAME_VERSION.saturating_sub(version.chars().count() + 1);
    let name_version = format!("{} {}", fit(name, for_name), version);
    let line = format!(
        "    {:<28}{:<32}{}",
        fit(&name_version, NAME_VERSION),
        fit(tag, 31),
        cells
    );
    line.trim_end().to_string()
}

/// One root→package path, wrapped at the terminal width with the continuation
/// hanging under the first segment. `indent` may carry a label (`[ 3] `),
/// replaced by blanks on continuation lines.
fn write_path(out: &mut String, indent: &str, path: &[PkgId]) {
    let segs: Vec<String> = path
        .iter()
        .map(|p| format!("{} {}", p.name, p.version))
        .collect();
    let hang = " ".repeat(indent.chars().count() + 2);
    let mut line = String::from(indent);
    let mut first = true;
    for (i, seg) in segs.iter().enumerate() {
        let arrow = if i + 1 == segs.len() { "" } else { " ->" };
        let piece = if first {
            format!("{seg}{arrow}")
        } else {
            format!(" {seg}{arrow}")
        };
        if !first && line.chars().count() + piece.chars().count() > W_LINE {
            let _ = writeln!(out, "{line}");
            line = format!("{hang}{seg}{arrow}");
        } else {
            line.push_str(&piece);
        }
        first = false;
    }
    let _ = writeln!(out, "{line}");
}

/// Physical `*.rs` lines under a directory via [`aterm_census::collect_rs_files`],
/// measured the way [`crate::loc`] measures, so the two can be subtracted.
fn rs_lines(dir: &Path) -> u64 {
    let mut files = Vec::new();
    if aterm_census::collect_rs_files(dir, &mut files).is_err() {
        return 0;
    }
    files
        .iter()
        .filter_map(|f| std::fs::read_to_string(f).ok())
        .map(|t| t.lines().count() as u64)
        .sum()
}

fn yes_no(b: bool) -> &'static str {
    if b { "YES" } else { "no" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::repo_root;

    fn toy() -> Graph {
        // root -> a -> c ; root -> b -> c ; c -> d
        let (r, a, b, c, d) = (
            PkgId::new("root", "1"),
            PkgId::new("a", "1"),
            PkgId::new("b", "1"),
            PkgId::new("c", "1"),
            PkgId::new("d", "1"),
        );
        let mut g = Graph {
            root: r.clone(),
            ..Graph::default()
        };
        for n in [&r, &a, &b, &c, &d] {
            g.nodes.insert(n.clone());
        }
        g.edges
            .entry(r.clone())
            .or_default()
            .extend([a.clone(), b.clone()]);
        g.edges.entry(a.clone()).or_default().insert(c.clone());
        g.edges.entry(b.clone()).or_default().insert(c.clone());
        g.edges.entry(c.clone()).or_default().insert(d.clone());
        g
    }

    #[test]
    fn every_path_is_counted_and_the_shortest_come_first() {
        let g = toy();
        let d = PkgId::new("d", "1");
        assert_eq!(count_paths(&g, &d), (2, false));
        let (paths, hit) = shortest_paths(&g, &d, 10);
        assert!(!hit);
        assert_eq!(paths.len(), 2);
        for p in &paths {
            assert_eq!(p.first(), Some(&g.root));
            assert_eq!(p.last(), Some(&d));
            assert_eq!(p.len(), 4);
        }
        // The root reaches itself by exactly one (empty) path.
        assert_eq!(count_paths(&g, &g.root), (1, false));
    }

    #[test]
    fn a_bounded_request_returns_only_the_shortest() {
        let g = toy();
        let (paths, _) = shortest_paths(&g, &PkgId::new("c", "1"), 1);
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].len(), 3);
    }

    #[test]
    fn an_absent_node_has_no_paths_and_no_panic() {
        let g = toy();
        let ghost = PkgId::new("ghost", "9");
        assert_eq!(shortest_paths(&g, &ghost, 10).0.len(), 0);
        assert_eq!(count_paths(&g, &ghost), (0, false));
    }

    #[test]
    fn the_workspace_patch_table_is_read_with_its_vendored_versions() {
        let forks = read_forks(&repo_root()).expect("the workspace manifest parses");
        assert!(
            forks.contains_key("indexmap"),
            "vendor/indexmap is patched in: {forks:?}"
        );
        let w = &forks["indexmap"][0];
        assert_eq!(w.path, "vendor/indexmap");
        let v = w
            .version
            .clone()
            .expect("vendor/indexmap/Cargo.toml carries a literal version");
        let root = repo_root();
        assert_eq!(
            patch_state(&root, None, &forks, &PkgId::new("indexmap", v.clone())),
            PatchState::Fork {
                path: "vendor/indexmap".into()
            }
        );
        // Any other version of a forked name is the defect this verb reports.
        // A synthetic id, since the shipped graph carries no unpatched sibling.
        assert!(matches!(
            patch_state(&root, None, &forks, &PkgId::new("indexmap", "3.0.0")),
            PatchState::UnpatchedBesideFork { .. }
        ));
        assert_eq!(
            patch_state(&root, None, &forks, &PkgId::new("serde", "1.0.0")),
            PatchState::Registry
        );
        // A first-party vendored crate has no patch entry; its measured
        // directory is what the roster decides on, so it is not reported as
        // crates.io.
        let own = crate::model::PkgFacts {
            root_dir: Some(root.join("vendor/astream/crates/astream-cap")),
            ..crate::model::PkgFacts::default()
        };
        assert_eq!(
            patch_state(
                &root,
                Some(&own),
                &forks,
                &PkgId::new("astream-cap", "0.0.0")
            ),
            PatchState::FirstPartyVendored {
                path: "vendor/astream/crates/astream-cap".into()
            }
        );
        // A REAL fork's directory must not be swallowed by the same arm.
        let fork = crate::model::PkgFacts {
            root_dir: Some(root.join("vendor/indexmap")),
            ..crate::model::PkgFacts::default()
        };
        assert!(matches!(
            patch_state(
                &root,
                Some(&fork),
                &forks,
                &PkgId::new("indexmap", "2.14.0")
            ),
            PatchState::Fork { .. }
        ));
    }

    /// The dir line's width depends on content, not environment: an in-repo
    /// dir loses the checkout prefix, and an out-of-repo registry dir is
    /// bounded while keeping the package directory.
    #[test]
    fn the_dir_line_is_bounded_and_keeps_the_package_directory() {
        let root = Path::new(
            "/private/tmp/build-501/-Users-example-aterm/2b813347-8abb-4e85/scratchpad/wt",
        );

        // IN-REPO: repo-relative, and the deep checkout prefix is gone.
        let line = dir_line(root, &root.join("vendor/indexmap"));
        assert_eq!(line, "vendor/indexmap");

        // OUT-OF-REPO: nothing to strip, so it must be bounded instead.
        let registry = Path::new(
            "/Users//a-very-long-operator-name/.cargo/registry/src/\
             index.crates.io-1949cf8c6b5b557f/wgpu-core-deps-windows-linux-android-29.0.3",
        );
        let line = dir_line(root, registry);
        assert!(
            line.chars().count() <= W_LINE - 16,
            "{} cols: {line:?}",
            line.chars().count()
        );
        assert!(
            line.ends_with("wgpu-core-deps-windows-linux-android-29.0.3"),
            "the package directory is the part worth keeping: {line:?}"
        );
        // …and the whole rendered line, indent included, fits the budget the
        // width test holds every line to.
        assert!(format!("                {line}").chars().count() <= 100);
    }

    /// A long name never runs into the next column.
    #[test]
    fn a_long_name_version_keeps_the_column_separator() {
        let row = versions_row(
            "wgpu-core-deps-windows-linux-android",
            "29.0.3",
            "registry",
            "linux, win",
        );
        assert!(
            !row.contains("29.0.3registry"),
            "the version ran into the tag: {row:?}"
        );
        assert!(
            row.chars().count() <= 100,
            "{} cols: {row:?}",
            row.chars().count()
        );
        // The fitted name is followed by a REAL separator, not butted up.
        let tag_at = row.find("registry").expect("the tag is printed");
        assert!(
            row[..tag_at].ends_with(' '),
            "no separator before the tag: {row:?}"
        );
        // The version must survive the cut.
        assert!(
            row.contains("29.0.3"),
            "the version was truncated away: {row:?}"
        );
        // A short row is untouched, padding and all.
        let short = versions_row("indexmap", "2.13.0", "registry", "mac-arm");
        assert!(short.starts_with("    indexmap 2.13.0 "), "{short:?}");
        assert!(short.contains(" registry"), "{short:?}");
    }

    #[test]
    fn wrapped_paths_stay_inside_the_terminal() {
        let mut out = String::new();
        let long: Vec<PkgId> = (0..12)
            .map(|i| PkgId::new(format!("a-rather-long-package-name-{i}"), "0.47.0"))
            .collect();
        write_path(&mut out, "      ", &long);
        for line in out.lines() {
            assert!(
                line.chars().count() <= 100,
                "{} cols: {line:?}",
                line.chars().count()
            );
        }
        assert!(out.contains("a-rather-long-package-name-11 0.47.0"));
    }

    /// What blame owes for a forked crate in the shipped graph: the version,
    /// that it is forked, the fork's path, the cells it resolves in, its
    /// dominator cost, and the first-party edges responsible.
    ///
    /// Liveness reporting is proved separately: `patch_state` returns
    /// `UnpatchedBesideFork` for a synthetic off-fork version in
    /// `the_workspace_patch_table_is_read_with_its_vendored_versions`, and
    /// `tests/red_fixtures.rs::an_unpatched_sibling_version_reds_the_forge_verb`
    /// plants one in a scratch tree.
    #[test]
    fn blame_names_the_fork_its_path_and_its_cost_in_every_cell() {
        let root = repo_root();
        // `indexmap` is resolved via wgpu in the linux cell, so blame is
        // exercised there against a live fork.
        let cells = ["linux".to_string()];
        let out = run(&root, "indexmap", &cells).expect("blame runs");
        assert!(out.ok, "{}", out.log);
        assert!(
            out.log.contains("indexmap 2.14.0"),
            "the version is named:\n{}",
            out.log
        );
        assert!(
            out.log.contains("FORKED"),
            "the fork is flagged as one:\n{}",
            out.log
        );
        assert!(
            out.log.contains("vendor/indexmap"),
            "the fork's path is named:\n{}",
            out.log
        );
        for c in &cells {
            assert!(
                out.log.contains(c.as_str()),
                "cell `{c}` is reported:\n{}",
                out.log
            );
            assert!(
                out.log.contains(&format!("cell {c}   dom ")),
                "cell `{c}` gets a dominator cost:\n{}",
                out.log
            );
        }
        assert!(
            out.log.contains("first-party edges responsible:"),
            "blame's whole job is naming the first-party edge:\n{}",
            out.log
        );
        // ONE version resolves, in every cell, so there is no liveness defect
        // to report. Asserted as a property of THIS fork rather than of the tree
        // as a whole — `[OB-12]` (`verify forge`) owns the tree-wide
        // claim, and `red_fixtures` owns the detector.
        assert!(
            !out.log.contains("PATCH LIVENESS DEFECT"),
            "indexmap resolves at one version, so blame must not report a \
             liveness defect:\n{}",
            out.log
        );
        for line in out.log.lines() {
            assert!(
                line.chars().count() <= 100,
                "{} cols: {line:?}",
                line.chars().count()
            );
        }
    }

    #[test]
    fn blame_accepts_an_exact_version_and_prints_paths_and_cost() {
        let root = repo_root();
        let out = run(&root, "libc@0.2.186", &["mac-arm".into()]).expect("blame runs");
        assert!(out.ok, "{}", out.log);
        assert!(out.log.contains("libc 0.2.186"));
        assert!(out.log.contains("path(s) from `aterm`"), "{}", out.log);
        assert!(
            out.log.contains("first-party edges responsible:"),
            "{}",
            out.log
        );
    }

    #[test]
    fn an_absent_package_is_refused_with_the_cells_it_searched() {
        let root = repo_root();
        let out = run(&root, "definitely-not-a-crate", &["mac-arm".into()]).expect("blame runs");
        assert!(!out.ok, "an absent package is a refusal, not a pass");
        assert!(out.log.contains("NOT RESOLVED"));
        assert!(
            out.log.contains("mac-arm"),
            "the refusal names the cells searched:\n{}",
            out.log
        );
        assert!(
            out.log.contains("Fix:"),
            "the refusal names the fix:\n{}",
            out.log
        );
    }

    #[test]
    fn a_wrong_version_of_a_present_package_says_which_versions_exist() {
        let root = repo_root();
        let out = run(&root, "libc@0.0.1", &["mac-arm".into()]).expect("blame runs");
        assert!(!out.ok);
        assert!(
            out.log.contains("IS resolved, but not at 0.0.1"),
            "{}",
            out.log
        );
    }

    #[test]
    fn a_malformed_spec_is_a_typed_refusal_naming_the_shape() {
        let root = repo_root();
        // `Outcome` carries no `Debug`, so the error arm is taken by pattern.
        let Err(err) = run(&root, "indexmap@", &["mac-arm".into()]) else {
            panic!("a spec with an empty version must be refused, not surveyed")
        };
        assert!(err.contains("name@version"), "{err}");
    }
}
