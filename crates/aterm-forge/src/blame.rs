// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Dependency path, dominator cost, and fork liveness report for one package.

use crate::model::{CellSurvey, Graph, PkgId};
use crate::survey::{commas, fit, fit_path, survey_cells, wrap};
use crate::{Outcome, dominator, resolve};
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};
use std::fmt::Write as _;
use std::path::Path;

/// Paths printed per cell before the rest are counted rather than listed.
const PATHS_SHOWN: usize = 10;
/// Hard bound on the best-first path search, so a wide graph cannot hang the report.
const SEARCH_BUDGET: usize = 40_000;
const W_LINE: usize = 96;

/// What `[patch.crates-io]` actually did to this exact `(name, version)`.
#[derive(Clone, Debug, PartialEq, Eq)]
enum PatchState {
    /// This version is the vendored fork aterm maintains.
    Fork { path: String },
    /// A fork of this name exists at another version, and this copy is the pristine registry code.
    UnpatchedBesideFork { path: String, fork_version: String },
    /// A fork directory exists but its version could not be read.
    ForkVersionUnreadable { path: String },
    /// Ordinary registry code.
    Registry,
    /// aterm's own code, vendored in-tree and reached by a path dependency.
    FirstPartyVendored { path: String },
}

#[derive(Clone, Debug, Default)]
struct VendorFork {
    path: String,
    version: Option<String>,
    /// Renamed row claiming one version line of its package ([`crate::policy::in_row_scope`]).
    renamed: bool,
}

/// Package → the patch rows replacing it ([`crate::policy::patched_package`]).
type Forks = BTreeMap<String, Vec<VendorFork>>;

/// Explain one package.
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

/// The patch-liveness check.
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
            let _ = writeln!(log, "                {}", dir_line(root, dir));
        }
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

/// The root's direct dependencies from which the target is still reachable.
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

/// Versions of `name` in `Cargo.lock`.
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

/// Cheap "did you mean".
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

/// The measured package's directory as the report shows it ([`fit_path`]).
fn dir_line(root: &Path, dir: &Path) -> String {
    let shown = dir.strip_prefix(root).unwrap_or(dir);
    fit_path(&shown.display().to_string(), W_LINE - 16)
}

/// One versions-resolved row.
fn versions_row(name: &str, version: &str, tag: &str, cells: &str) -> String {
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

/// One root→package path.
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

/// Physical `*.rs` line count under a directory ([`crate::loc`], [`aterm_census::collect_rs_files`]).
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

const fn yes_no(b: bool) -> &'static str {
    if b { "YES" } else { "no" }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toy() -> Graph {
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
        assert_eq!(count_paths(&g, &g.root), (1, false));
        let (paths, _) = shortest_paths(&g, &PkgId::new("c", "1"), 1);
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].len(), 3);
        let ghost = PkgId::new("ghost", "9");
        assert_eq!(shortest_paths(&g, &ghost, 10).0.len(), 0);
        assert_eq!(count_paths(&g, &ghost), (0, false));
    }
}
