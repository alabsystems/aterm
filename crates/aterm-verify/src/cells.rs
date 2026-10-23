// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `verify cells`: cross-cell lint and type-check across Windows, linux-arm, and wasm32.

//! STOCK EXCEPTION: the Trust sysroot carries only the host std, so each cell
use aterm_uds::output::CommandOutput as _;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Instant;

use aterm_forge::model::{Cell, Graph, PkgId};

use crate::Ctx;
use crate::exec::{self, Cmd};
use crate::ladder::{Outcome, Report, Severity};
use crate::plan::{Lane, lane_dir};
use crate::rustup;
use crate::stages::lane_jobs;

/// The build-script shims a cell may apply.
pub const POLICY: &str = "tools/cross-cell-gate.tsv";

/// The triples some machine in this fleet runs natively.
pub const FLEET_HOST_TRIPLES: [&str; 3] = [
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-unknown-linux-gnu",
];

/// The clippy config the browser cells lint with: std's clock panics on
const WASM_CLIPPY_CONF: &str = "crates/aterm-time/wasm32";

/// The cells this check compiles by default: forge's own list, less the hosted triples.
#[must_use]
pub fn foreign_cells() -> Vec<Cell> {
    aterm_forge::resolve::default_cells()
        .into_iter()
        .filter(|c| !FLEET_HOST_TRIPLES.contains(&c.triple.as_str()))
        .collect()
}

/// The cells a run checks: each `--cell` named, else [`foreign_cells`].
///
/// # Errors
/// A name forge's cell list does not have.
pub fn selected(names: &[String]) -> Result<Vec<Cell>, String> {
    if names.is_empty() {
        return Ok(foreign_cells());
    }
    aterm_forge::resolve::select(&aterm_forge::resolve::default_cells(), names)
}

/// The `host:` line of `rustc -vV`.
#[must_use]
pub fn host_of(version: &str) -> Option<&str> {
    version
        .lines()
        .find_map(|l| l.strip_prefix("host:"))
        .map(str::trim)
        .filter(|h| !h.is_empty())
}

/// This machine's triple, as the run's `trustc` reports it.
fn host_triple(ctx: &Ctx) -> Result<String, String> {
    let trustc = ctx.tools.stage2_dir.join("trustc");
    let out = aterm_uds::spawnfd::command(&trustc)
        .arg("-vV")
        .output_owned()
        .map_err(|e| format!("could not run `{} -vV`: {e}", trustc.display()))?;
    let text = String::from_utf8_lossy(&out.stdout);
    host_of(&text)
        .map(str::to_string)
        .ok_or_else(|| format!("`{} -vV` named no host triple", trustc.display()))
}

/// One `cshim` row: a C-bearing build script replaced, for one triple, by cargo's own override on the command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shim {
    pub package: String,
    pub version: String,
    pub triple: String,
    /// The package's `links` key, which cargo's override table is keyed by.
    pub links: String,
}

impl Shim {
    /// The `--config` value that stands in for the script.
    #[must_use]
    pub fn config(&self) -> String {
        format!(
            "target.{}.{}.rustc-link-lib=[\"{}\"]",
            self.triple, self.links, self.links
        )
    }
}

/// [`POLICY`], parsed: `cshim <name>@<version> <triple> <links> <why>`, TAB-separated, `#` and blank lines ignored.
///
/// # Errors
/// The first row that is not a well-formed `cshim` for one non-wasm triple.
pub fn parse_policy(text: &str) -> Result<Vec<Shim>, String> {
    let mut shims = Vec::new();
    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim_end();
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let at = format!("{POLICY}:{}", n + 1);
        let cols: Vec<&str> = line.split('\t').collect();
        if cols[0] != "cshim" {
            return Err(format!(
                "{at}: unknown row kind `{}` — the only kind is `cshim`",
                cols[0]
            ));
        }
        if cols.len() < 5 {
            return Err(format!(
                "{at}: a `cshim` row takes 5 TAB-separated columns, found {}",
                cols.len()
            ));
        }
        let Some((package, version)) = cols[1].split_once('@') else {
            return Err(format!(
                "{at}: a `cshim` key is `name@version`, not `{}`",
                cols[1]
            ));
        };
        let triple = cols[2];
        if triple == "*" {
            return Err(format!(
                "{at}: a `cshim` row names one triple; what a script emits depends on it"
            ));
        }
        if triple.starts_with("wasm32") || triple.contains("hermit") {
            return Err(format!(
                "{at}: no `cshim` on `{triple}`, where a bundled-C build script emits rustc-cfg"
            ));
        }
        shims.push(Shim {
            package: package.to_string(),
            version: version.to_string(),
            triple: triple.to_string(),
            links: cols[3].to_string(),
        });
    }
    Ok(shims)
}

/// What one cell's run decided.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CellOutcome {
    /// Every obligation held: the row's summary.
    Checked(String),
    /// Nothing was compiled: why, and the install that clears it.
    Skipped { why: String, fix: String },
    /// The tree did not hold for the cell: the first cause, and the lines that say what went wrong.
    Failed { status: String, detail: Vec<String> },
    /// The machine could not answer for the cell (no graph, no toolchain, a child that never ran): what failed, and how.
    CouldNotRun { status: String, detail: Vec<String> },
}

/// The `cells` check's stage: one row per selected cell, then the `cshim` rows nothing could use.
pub fn run(ctx: &Ctx, r: &mut Report) {
    let policy = match std::fs::read_to_string(ctx.root.join(POLICY))
        .map_err(|e| format!("cannot read {POLICY}: {e}"))
        .and_then(|t| parse_policy(&t))
    {
        Ok(p) => p,
        Err(why) => {
            r.cannot_run(format!("cells — {why}"));
            return;
        }
    };
    let cells = match selected(&ctx.cells) {
        Ok(c) => c,
        Err(why) => {
            r.cannot_run(format!("cells — {why}"));
            return;
        }
    };
    let host = match host_triple(ctx) {
        Ok(h) => h,
        Err(why) => {
            r.cannot_run(format!("cells — this machine's triple: {why}"));
            return;
        }
    };
    let rustup = rustup::find_rustup_in(
        Some(ctx.env.path.as_os_str()),
        ctx.env.cargo_home.as_deref().map(Path::as_os_str),
        Some(ctx.env.home.as_os_str()),
    );
    let mut members: Option<Result<BTreeSet<String>, String>> = None;
    for cell in &cells {
        let label = format!("cell {} ({})", cell.name, cell.triple);
        let started = Instant::now();
        let outcome = if cell.triple == host {
            CellOutcome::Skipped {
                why: "this machine's own triple, judged by its own build".to_string(),
                fix: "targo --unverified verify lint test".to_string(),
            }
        } else if let Some(rustup) = &rustup {
            check_cell(ctx, rustup, cell, &policy, &mut members)
        } else {
            CellOutcome::Skipped {
                why: format!("no rustup {}", rustup::RUSTUP_LOOKED),
                fix: format!(
                    "{}, with stable's {} std",
                    aterm_uds::host::tool("rustup").remedy(),
                    cell.triple
                ),
            }
        };
        let took = format!("{label}: {}s", started.elapsed().as_secs());
        match outcome {
            CellOutcome::Checked(summary) => {
                r.pass(format!("{label} — {summary}"));
                r.raw(took);
            }
            CellOutcome::Skipped { why, fix } => r.skip(format!("{label} — {why} (fix: {fix})")),
            CellOutcome::Failed { status, detail } => {
                r.fail(format!("{label} — {status}"));
                r.raw(took);
                r.raw(detail.join("\n"));
            }
            CellOutcome::CouldNotRun { status, detail } => {
                r.cannot_run(format!("{label} — {status}"));
                r.raw(took);
                r.raw(detail.join("\n"));
            }
        }
    }
    if runs_win32_oracles(&cells) {
        win32_oracles(ctx, r, rustup.is_some());
    }
    audit_policy(ctx, r, &policy, &cells);
}

/// The windows-sys oracles, `aterm-win32-oracle`'s two verbs (WB.1, WB.2 and WB.3 of docs/AUDIT-third-party-2026-10-15.md), each with what it holds to the registry crates of its version. (STOCK EXCEPTION)
/// either Windows cell runs each once. The same STOCK EXCEPTION as the cells:
pub const WIN32_ORACLES: [(&str, &str); 2] = [
    (
        "oracle",
        "the windows-sys 0.61 facade and aterm-win32's closure against registry windows-sys 0.61.2, its COM and DXGI against windows 0.62.2",
    ),
    (
        "oracle-052",
        "the windows-sys 0.52 facade (crates/aterm-windows-sys-052) against registry windows-sys 0.52.0",
    ),
];

/// Does this run check a Windows cell, which brings the [`WIN32_ORACLES`]?
#[must_use]
pub fn runs_win32_oracles(cells: &[Cell]) -> bool {
    cells.iter().any(|c| c.triple.ends_with("-pc-windows-msvc"))
}

/// `targo --unverified run -q -p aterm-win32-oracle --target-dir <cells lane>/win32-oracle-bin -- <verb> --root <R>`, from `<R>`.
///
/// # Errors
/// The current-directory error from making a relative path absolute.
pub fn win32_oracle_cmd(ctx: &Ctx, verb: &str) -> std::io::Result<Cmd> {
    let root = std::path::absolute(&ctx.root)?;
    let lane = std::path::absolute(
        lane_dir(ctx, Lane::CellsTarget).unwrap_or_else(|| ctx.target_dir.clone()),
    )?;
    let mut cmd = Cmd::new(&ctx.tools.targo).args(
        [
            "--unverified",
            "run",
            "-q",
            "-p",
            "aterm-win32-oracle",
            "--target-dir",
        ]
        .map(String::from)
        .into_iter()
        .chain([
            lane.join("win32-oracle-bin").display().to_string(),
            "--".to_string(),
            verb.to_string(),
            "--root".to_string(),
            root.display().to_string(),
        ]),
    );
    cmd.cwd = Some(root);
    Ok(cmd)
}

/// The reason an oracle that decided nothing gives on its last `… could not run: <reason>` line.
#[must_use]
pub fn win32_oracle_not_run_reason(output: &str) -> Option<String> {
    output
        .lines()
        .rev()
        .find_map(|l| l.split_once(" could not run: ").map(|(_, why)| why.trim()))
        .filter(|why| !why.is_empty())
        .map(str::to_string)
}

/// One row per [`WIN32_ORACLES`] verb. The oracle's exit is read as the libc
fn win32_oracles(ctx: &Ctx, r: &mut Report, have_rustup: bool) {
    for (verb, holds) in WIN32_ORACLES {
        let label = format!("win32 {verb} ({holds}; x86_64 and aarch64 pc-windows-msvc)");
        if !ctx.tools.have_targo() {
            r.skip(format!("{label} — no targo to build the driver"));
            continue;
        }
        if !have_rustup {
            r.skip(format!(
                "{label} — no rustup {} (fix: {}, with stable's x86_64-pc-windows-msvc and \
                 aarch64-pc-windows-msvc stds)",
                rustup::RUSTUP_LOOKED,
                aterm_uds::host::tool("rustup").remedy()
            ));
            continue;
        }
        let cmd = match win32_oracle_cmd(ctx, verb) {
            Ok(cmd) => cmd,
            Err(error) => {
                r.cannot_run(format!("{label} — cannot resolve the checkout: {error}"));
                continue;
            }
        };
        let started = Instant::now();
        let out = exec::run(&cmd, ctx.exec_env());
        r.raw(out.output.as_str());
        r.raw(format!("win32 {verb}: {}s", started.elapsed().as_secs()));
        let outcome = crate::stages::libc_oracle_outcome(&out);
        let why = if outcome == Outcome::Fail(Severity::CouldNotRun) {
            out.environment_failure()
                .or_else(|| win32_oracle_not_run_reason(&out.output))
        } else {
            None
        };
        match why {
            Some(why) => r.cannot_run(format!("{label} — {why}")),
            None => r.record_child(&out, outcome, label),
        }
    }
}

/// The `cshim` rows no cell of this run used: one naming a triple no forge
fn audit_policy(ctx: &Ctx, r: &mut Report, policy: &[Shim], ran: &[Cell]) {
    let all = aterm_forge::resolve::default_cells();
    let mut graphs: BTreeMap<String, Result<(Graph, BTreeSet<PkgId>), String>> = BTreeMap::new();
    let mut members: Option<Result<BTreeSet<String>, String>> = None;
    let mut unread = false;
    for shim in policy
        .iter()
        .filter(|s| !ran.iter().any(|c| c.triple == s.triple))
    {
        let Some(cell) = all.iter().find(|c| c.triple == shim.triple) else {
            r.fail(format!(
                "{POLICY} shims `{}@{}` on {}, which is no cell's triple — delete the row",
                shim.package, shim.version, shim.triple
            ));
            continue;
        };
        if !ctx.cells.is_empty() {
            unread = true;
            continue;
        }
        let graph = graphs.entry(cell.name.clone()).or_insert_with(|| {
            graph_of(ctx, cell).map(|(g, paths)| {
                let library = library_nodes(ctx, &g, &paths);
                (g, library)
            })
        });
        match graph {
            Ok((g, library)) => {
                if dead_shim(shim, library, &BTreeSet::new()).is_none() {
                    continue;
                }
                let names: BTreeSet<String> = g.nodes.iter().map(|p| p.name.clone()).collect();
                let tests = members
                    .get_or_insert_with(|| member_names(ctx))
                    .clone()
                    .and_then(|m| test_graph(ctx, cell, &m, &names));
                match tests {
                    Ok(t) => {
                        if let Some(why) = dead_shim(shim, library, &t.nodes) {
                            r.fail(why);
                        }
                    }
                    Err(e) => r.cannot_run(format!(
                        "{POLICY} shims `{}@{}` on {}, and cell {}'s test targets did not \
                         resolve: {}",
                        shim.package,
                        shim.version,
                        shim.triple,
                        cell.name,
                        e.lines().next().unwrap_or_default()
                    )),
                }
            }
            Err(e) => r.cannot_run(format!(
                "{POLICY} shims `{}@{}` on {}, and cell {} did not resolve: {}",
                shim.package,
                shim.version,
                shim.triple,
                cell.name,
                e.lines().next().unwrap_or_default()
            )),
        }
    }
    if unread {
        r.raw(format!(
            "{POLICY}: the rows of cells this run did not check were not read (--cell)"
        ));
    }
}

/// Whether `shim` is live in the lint's graph and in the test-target pass's: its package at its version is in each.
#[must_use]
pub fn shim_lanes(shim: &Shim, library: &BTreeSet<PkgId>, tests: &BTreeSet<PkgId>) -> (bool, bool) {
    let live = |nodes: &BTreeSet<PkgId>| {
        nodes
            .iter()
            .any(|id| id.name == shim.package && id.version == shim.version)
    };
    (live(library), live(tests))
}

/// `Some(why)` when `shim`'s version is in neither pass's graph, which a
fn dead_shim(shim: &Shim, library: &BTreeSet<PkgId>, tests: &BTreeSet<PkgId>) -> Option<String> {
    let versions: BTreeSet<&str> = library
        .iter()
        .chain(tests)
        .filter(|id| id.name == shim.package)
        .map(|id| id.version.as_str())
        .collect();
    (!versions.contains(shim.version.as_str())).then(|| {
        let versions: Vec<&str> = versions.into_iter().collect();
        format!(
            "{POLICY} shims `{}@{}` on {}, and this cell's lint and test-target graphs carry \
             {}: re-read that version's build script and move the row, or delete it",
            shim.package,
            shim.version,
            shim.triple,
            if versions.is_empty() {
                "no such package".to_string()
            } else {
                versions.join(", ")
            }
        )
    })
}

/// What the all-targets pass compiles: the members it selects and excludes, and their target dependencies, dev-dependencies included.
#[derive(Debug, Default)]
pub struct TestGraph {
    pub nodes: BTreeSet<PkgId>,
    pub selected: Vec<String>,
    pub excluded: Vec<String>,
}

/// The test-target pass's graph for `cell`: the selection
fn test_graph(
    ctx: &Ctx,
    cell: &Cell,
    members: &BTreeSet<String>,
    normal: &BTreeSet<String>,
) -> Result<TestGraph, String> {
    let patched = patched_crates(&ctx.root)?;
    let (selected, excluded) = test_selection(members, normal, &patched);
    if selected.is_empty() {
        return Err(format!(
            "no workspace member is in cell `{}`'s graph, so its all-targets pass would \
             compile nothing",
            cell.name
        ));
    }
    let (targo, root, path, triple) = (
        ctx.tools.targo.clone(),
        ctx.root.clone(),
        ctx.path_env.clone(),
        cell.triple.clone(),
    );
    let out = exec::bounded(ctx.child_ceiling, {
        let excluded = excluded.clone();
        move || test_tree(&targo, &path, &root, &triple, &excluded)
    })
    .unwrap_or_else(|why| Err(format!("`targo tree` {}", why.sentence())))?;
    let excluded_set: BTreeSet<String> = excluded.iter().cloned().collect();
    let roots: BTreeSet<String> = members.difference(&excluded_set).cloned().collect();
    Ok(TestGraph {
        nodes: parse_test_forest(&out, &roots)?,
        selected,
        excluded,
    })
}

/// `targo tree` over the workspace less `excluded`, for `triple`, with normal
fn test_tree(
    targo: &Path,
    path: &std::ffi::OsStr,
    root: &Path,
    triple: &str,
    excluded: &[String],
) -> Result<String, String> {
    let mut cmd = aterm_uds::spawnfd::command(targo);
    cmd.env("PATH", path)
        .current_dir(root)
        .arg("tree")
        .arg("--manifest-path")
        .arg(root.join("Cargo.toml"))
        .args(["--workspace", "--edges", "normal,dev,no-proc-macro"])
        .args(["--prefix", "depth", "--no-dedupe", "--locked", "--offline"])
        .args(["--target", triple]);
    for name in excluded {
        cmd.arg("--exclude").arg(name);
    }
    let out = cmd
        .output_owned()
        .map_err(|e| format!("could not run {} tree: {e}", targo.display()))?;
    if !out.status.success() {
        return Err(format!(
            "`targo tree` for {triple}'s test targets failed: {}",
            String::from_utf8_lossy(&out.stderr).trim_end()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The packages of a depth-prefixed `targo tree` forest whose roots must be exactly `roots`.
///
/// # Errors
/// A line forge cannot read, or a forest that is not the one asked for.
pub fn parse_test_forest(text: &str, roots: &BTreeSet<String>) -> Result<BTreeSet<PkgId>, String> {
    /// One depth-0 tree: whether its root is a proc macro, and every line from
    struct RootTree {
        proc_macro: bool,
        lines: Vec<(usize, PkgId)>,
    }
    let mut trees: Vec<RootTree> = Vec::new();
    let mut ancestors = 0;
    for raw in text.lines() {
        let Some(line) = aterm_forge::resolve::parse_line(raw)? else {
            continue;
        };
        if line.depth > ancestors {
            return Err(format!("the test dependency tree jumps a depth at `{raw}`"));
        }
        if line.depth == 0 {
            trees.push(RootTree {
                proc_macro: line.is_proc_macro,
                lines: Vec::new(),
            });
        }
        let Some(tree) = trees.last_mut() else {
            return Err(format!(
                "the test dependency tree starts below a root at `{raw}`"
            ));
        };
        tree.lines.push((line.depth, line.id));
        ancestors = line.depth + 1;
    }
    let subtree = |lines: &[(usize, PkgId)], at: usize| -> BTreeSet<PkgId> {
        let depth = lines[at].0;
        let mut nodes = BTreeSet::from([lines[at].1.clone()]);
        for (d, id) in &lines[at + 1..] {
            if *d <= depth {
                break;
            }
            nodes.insert(id.clone());
        }
        nodes
    };
    let mut host: BTreeMap<String, Vec<BTreeSet<PkgId>>> = BTreeMap::new();
    for tree in trees.iter().filter(|tree| tree.proc_macro) {
        for at in 1..tree.lines.len() {
            host.entry(tree.lines[at].1.name.clone())
                .or_default()
                .push(subtree(&tree.lines, at));
        }
    }
    let mut by_root: BTreeMap<String, Vec<(bool, BTreeSet<PkgId>)>> = BTreeMap::new();
    for tree in &trees {
        by_root
            .entry(tree.lines[0].1.name.clone())
            .or_default()
            .push((tree.proc_macro, subtree(&tree.lines, 0)));
    }
    let seen: BTreeSet<String> = by_root.keys().cloned().collect();
    let mut nodes = BTreeSet::new();
    for (name, mut copies) in by_root {
        if copies.len() > 1 {
            let Some(host_copies) = host.get(&name) else {
                return Err(format!(
                    "the test dependency tree repeats root `{name}`, and no proc macro in \
                     the forest reaches it (so neither copy is known to be the host's)"
                ));
            };
            let mut extra = copies.len() - 1;
            copies.retain(|(_, set)| {
                if extra > 0 && host_copies.contains(set) {
                    extra -= 1;
                    false
                } else {
                    true
                }
            });
            if copies.len() != 1 {
                return Err(format!(
                    "the test dependency tree repeats root `{name}`, and its copies do not \
                     reduce to one target copy beside the host's its proc macro prints"
                ));
            }
        }
        let (proc_macro, set) = copies.remove(0);
        if !proc_macro {
            nodes.extend(set);
        }
    }
    if seen != *roots {
        return Err(format!(
            "the test dependency tree's roots {seen:?} are not the pass's selected members \
             {roots:?}"
        ));
    }
    Ok(nodes)
}

/// The cell's packages compiled for the host ([`host_side`]). A third-party
fn host_side_of(ctx: &Ctx, graph: &Graph, paths: &BTreeMap<PkgId, PathBuf>) -> BTreeSet<PkgId> {
    host_side(graph, |id| {
        aterm_forge::loc::package_dir(&ctx.root, id, paths.get(id).map(PathBuf::as_path))
            .is_some_and(|d| is_proc_macro(&d))
    })
}

/// What a cell's lint compiles for its triple: its graph less the host side.
fn library_nodes(ctx: &Ctx, graph: &Graph, paths: &BTreeMap<PkgId, PathBuf>) -> BTreeSet<PkgId> {
    let host = host_side_of(ctx, graph, paths);
    graph.nodes.difference(&host).cloned().collect()
}

/// One cell's graph from forge's own resolver, asking this run's targo, under
fn graph_of(ctx: &Ctx, cell: &Cell) -> Result<(Graph, BTreeMap<PkgId, PathBuf>), String> {
    let (cargo, root, owned) = (ctx.forge_cargo(), ctx.root.clone(), cell.clone());
    crate::exec::bounded(ctx.child_ceiling, move || {
        let mut log = String::new();
        aterm_forge::resolve::graph_and_paths(&cargo, &root, &owned, &mut log)
    })
    .unwrap_or_else(|why| Err(format!("`targo tree` {}", why.sentence())))
}

/// One cell, end to end: its graph, its shims, its toolchain, the lint of
fn check_cell(
    ctx: &Ctx,
    rustup: &Path,
    cell: &Cell,
    policy: &[Shim],
    members: &mut Option<Result<BTreeSet<String>, String>>,
) -> CellOutcome {
    let failed = |status: &str, detail: Vec<String>| CellOutcome::Failed {
        status: status.to_string(),
        detail,
    };
    let could_not = |status: &str, detail: Vec<String>| CellOutcome::CouldNotRun {
        status: status.to_string(),
        detail,
    };
    let (graph, paths) = match graph_of(ctx, cell) {
        Ok(g) => g,
        Err(e) => return could_not("UNRESOLVED", vec![e]),
    };
    let graph_names: BTreeSet<String> = graph.nodes.iter().map(|p| p.name.clone()).collect();
    let in_repo = |id: &PkgId| {
        paths
            .get(id)
            .is_some_and(|dir| path_is_inside(dir, &ctx.root))
    };
    let own: BTreeSet<String> = graph
        .nodes
        .iter()
        .filter(|id| in_repo(id))
        .map(|id| id.name.clone())
        .collect();
    let proc_macros: BTreeSet<String> = graph
        .nodes
        .iter()
        .filter(|id| in_repo(id) && paths.get(*id).is_some_and(|d| is_proc_macro(d)))
        .map(|id| id.name.clone())
        .collect();
    let host = host_side_of(ctx, &graph, &paths);
    let host_deps: BTreeSet<String> = host
        .iter()
        .map(|id| id.name.clone())
        .filter(|name| own.contains(name) && !proc_macros.contains(name))
        .collect();
    let not_owed: BTreeSet<String> = proc_macros.union(&host_deps).cloned().collect();
    let library: BTreeSet<PkgId> = graph.nodes.difference(&host).cloned().collect();

    let tests = if cell.triple.starts_with("wasm32") {
        None
    } else {
        let members = members.get_or_insert_with(|| member_names(ctx));
        let members = match members {
            Ok(m) => m,
            Err(e) => return could_not("NO-MEMBERS", vec![e.clone()]),
        };
        match test_graph(ctx, cell, members, &graph_names) {
            Ok(t) => Some(t),
            Err(e) => return could_not("NO-TEST-GRAPH", vec![e]),
        }
    };
    let test_nodes = tests.as_ref().map(|t| t.nodes.clone()).unwrap_or_default();

    let (mut shim_args, mut shimmed) = (Vec::new(), Vec::new());
    let (mut test_shim_args, mut test_shimmed) = (Vec::new(), Vec::new());
    for shim in policy.iter().filter(|s| s.triple == cell.triple) {
        if let Some(why) = dead_shim(shim, &library, &test_nodes) {
            return failed("SHIM-DEAD", vec![why]);
        }
        let (library_live, tests_live) = shim_lanes(shim, &library, &test_nodes);
        if library_live {
            shim_args.push(shim.config());
            shimmed.push(shim.package.clone());
        }
        if tests_live {
            test_shim_args.push(shim.config());
            test_shimmed.push(shim.package.clone());
        }
    }

    let toolchain = match rustup::with_std(rustup, &cell.triple) {
        Ok(Some(tc)) if tc.clippy => tc,
        Ok(Some(tc)) => {
            return CellOutcome::Skipped {
                why: format!("`{}` has the std but no clippy", tc.name),
                fix: format!("rustup component add clippy --toolchain {}", tc.name),
            };
        }
        Ok(None) => {
            return CellOutcome::Skipped {
                why: "no rustup toolchain has its std".to_string(),
                fix: format!("rustup target add {} --toolchain stable", cell.triple),
            };
        }
        Err(e) => return could_not("NO-TOOLCHAIN", vec![e]),
    };

    let target_dir = crate::plan::cells_dir(ctx).join(&cell.name);
    let cwd = ctx.scratch.join(format!("cells-{}", cell.name));
    if let Err(e) = std::fs::create_dir_all(&cwd) {
        return could_not(
            "NO-CWD",
            vec![format!("could not create {}: {e}", cwd.display())],
        );
    }
    let cargo = CellCargo {
        ctx,
        triple: &cell.triple,
        toolchain: &toolchain,
        cwd: &cwd,
        target_dir: &target_dir,
        shim_args: &shim_args,
    };

    let lint = exec::run(&cargo.lint(cell), ctx.exec_env());
    let mut red = Red::default();
    let read = match read_cargo_run(&lint, &cell.triple, &graph_names, false, &mut red) {
        Ok(read) => read,
        Err(outcome) => return outcome,
    };
    for pkg in shimmed.iter().filter(|p| !read.checked.contains(*p)) {
        red.note(
            format!("SHIM-INERT({pkg})"),
            format!(
                "{POLICY} shims `{pkg}` and it still did not type-check: check the row's `links` \
                 key against the package's manifest (cargo ignores an override keyed to a name \
                 no package links)"
            ),
        );
    }
    let owed: Vec<&String> = own.difference(&not_owed).collect();
    let unread: Vec<&str> = owed
        .iter()
        .filter(|n| !read.checked.contains(**n))
        .map(|n| n.as_str())
        .collect();
    if !unread.is_empty() {
        red.note(
            format!("OWN-CODE-UNREAD({})", unread.len()),
            format!(
                "{} of aterm's {} crates in this graph were not type-checked for {}: {}",
                unread.len(),
                owed.len(),
                cell.triple,
                unread.join(", ")
            ),
        );
    }
    let mut summary = format!(
        "{}/{} own crates linted on `{}`{}",
        owed.len() - unread.len(),
        owed.len(),
        toolchain.name,
        not_owed_note(&proc_macros, &host_deps)
    );
    if !shimmed.is_empty() {
        summary.push_str(&format!(", shimmed (not linked): {}", shimmed.join(", ")));
    }
    if !test_shimmed.is_empty() {
        summary.push_str(&format!(
            ", test targets shimmed (not linked): {}",
            test_shimmed.join(", ")
        ));
    }

    if let (None, Some(tests)) = (&red.status, &tests) {
        let test_cargo = CellCargo {
            shim_args: &test_shim_args,
            ..cargo
        };
        let run = exec::run(&test_cargo.all_targets(&tests.excluded), ctx.exec_env());
        let read = match read_cargo_run(&run, &cell.triple, &graph_names, true, &mut red) {
            Ok(read) => read,
            Err(outcome) => return outcome,
        };
        let dependency_names: BTreeSet<String> =
            tests.nodes.iter().map(|id| id.name.clone()).collect();
        let dependencies = read_cargo_json(&run.output, &cell.triple, &dependency_names, false);
        for pkg in test_shimmed
            .iter()
            .filter(|p| !dependencies.checked.contains(*p))
        {
            red.note(
                format!("SHIM-INERT({pkg})"),
                format!(
                    "{POLICY} shims `{pkg}` for the test targets and it left no artifact for {}: \
                     check the row's `links` key against the package's manifest",
                    cell.triple
                ),
            );
        }
        let owed: Vec<&String> = tests
            .selected
            .iter()
            .filter(|n| !proc_macros.contains(*n))
            .collect();
        let missing: Vec<&str> = owed
            .iter()
            .filter(|n| !read.checked.contains(**n))
            .map(|n| n.as_str())
            .collect();
        if !missing.is_empty() {
            red.note(
                format!("TESTS-UNREAD({})", missing.len()),
                format!(
                    "{} of {} members left no test-target artifact for {}: {}",
                    missing.len(),
                    owed.len(),
                    cell.triple,
                    missing.join(", ")
                ),
            );
        }
        summary.push_str(&format!(
            ", {}/{} members' targets type-checked",
            owed.len() - missing.len(),
            owed.len()
        ));
    }
    match red.status {
        None => CellOutcome::Checked(summary),
        Some(status) => CellOutcome::Failed {
            status,
            detail: red.detail,
        },
    }
}

/// A cell's failures as they are found: the first cause names the row.
#[derive(Default)]
struct Red {
    status: Option<String>,
    detail: Vec<String>,
}

impl Red {
    fn note(&mut self, status: String, line: String) {
        self.status.get_or_insert(status);
        self.detail.push(line);
    }
}

/// One compiler pass's diagnostics, shared by the lint and all-targets pass.
fn read_cargo_run(
    run: &exec::Run,
    triple: &str,
    graph: &BTreeSet<String>,
    tests: bool,
    red: &mut Red,
) -> Result<CargoRead, CellOutcome> {
    if let Some(why) = run.environment_failure() {
        return Err(CellOutcome::CouldNotRun {
            status: "COULD-NOT-RUN".to_string(),
            detail: vec![why],
        });
    }
    let read = read_cargo_json(&run.output, triple, graph, tests);
    let n = read.errors.len();
    for (pkg, rendered) in read.errors.iter().take(20) {
        red.note(
            format!(
                "{}({n})",
                if tests {
                    "TEST-TARGET-ERROR"
                } else {
                    "TYPE-ERROR"
                }
            ),
            format!("in `{pkg}`:\n{}", rendered.trim_end()),
        );
    }
    let warnings: &[(String, String)] = if judges_warnings(triple) {
        &read.warnings
    } else {
        &[]
    };
    let n = warnings.len();
    for (pkg, rendered) in warnings.iter().take(20) {
        red.note(
            format!(
                "{}({n})",
                if tests {
                    "TEST-TARGET-WARNING"
                } else {
                    "WARNING"
                }
            ),
            format!("in `{pkg}`:\n{}", rendered.trim_end()),
        );
    }
    for line in cargo_level_errors(&run.output) {
        red.note(
            if tests {
                "TESTS-CARGO-ERROR"
            } else {
                "CARGO-ERROR"
            }
            .to_string(),
            line,
        );
    }
    if !run.ok && red.status.is_none() {
        red.note(
            if tests {
                "TESTS-CARGO-EXIT"
            } else {
                "CARGO-EXIT"
            }
            .to_string(),
            format!(
                "cargo did not exit successfully (exit {:?}):\n{}",
                run.code,
                run.output.trim_end()
            ),
        );
    }
    Ok(read)
}

/// The two cargo runs one cell makes, sharing its toolchain, cwd, target
struct CellCargo<'a> {
    ctx: &'a Ctx,
    triple: &'a str,
    toolchain: &'a rustup::Toolchain,
    cwd: &'a Path,
    target_dir: &'a Path,
    shim_args: &'a [String],
}

impl CellCargo<'_> {
    /// `<toolchain cargo> <verb> --target <triple> …` with this cell's
    fn base(&self, verb: &str) -> Cmd {
        let mut path = OsString::new();
        if let Some(bin) = self.toolchain.cargo.parent() {
            path.push(bin);
            path.push(":");
        }
        path.push(&self.ctx.path_env);
        let mut cmd = Cmd::new(&self.toolchain.cargo)
            .env("RUSTC", &self.toolchain.rustc)
            .env("RUSTUP_TOOLCHAIN", &self.toolchain.name)
            .env("PATH", path)
            .env("CARGO_TARGET_DIR", self.target_dir)
            .args([verb, "--target", self.triple])
            .args([
                "--locked",
                "--keep-going",
                "--message-format=json",
                "--manifest-path",
            ])
            .arg(self.ctx.root.join("Cargo.toml"))
            .demoted();
        cmd.cwd = Some(self.cwd.to_path_buf());
        if let Some(jobs) = lane_jobs(Lane::CellsTarget, self.ctx.build_jobs_ceiling()) {
            cmd = cmd.env("CARGO_BUILD_JOBS", jobs.to_string());
        }
        if let Some((var, value)) = self.toolchain.loader_env() {
            cmd = cmd.env(var, value);
        }
        for arg in self.shim_args {
            cmd = cmd.arg("--config").arg(arg);
        }
        cmd
    }

    /// The lint of the cell's root package: of clippy, only the `clippy.toml`
    fn lint(&self, cell: &Cell) -> Cmd {
        let mut cmd = self
            .base("clippy")
            .args(["-p", cell.package.as_str(), "--"])
            .args(lint_flags(self.triple));
        if self.triple.starts_with("wasm32") {
            cmd = cmd.env("CLIPPY_CONF_DIR", self.ctx.root.join(WASM_CLIPPY_CONF));
        }
        cmd
    }

    /// Every target of the workspace less `excluded`, type-checked.
    fn all_targets(&self, excluded: &[String]) -> Cmd {
        let mut cmd = self.base("check").args(["--all-targets", "--workspace"]);
        for name in excluded {
            cmd = cmd.arg("--exclude").arg(name);
        }
        cmd
    }
}

/// What a foreign cell's lint denies after `--`: the `clippy.toml` bans (and, off wasm, an `#[expect]` no ban fulfils).
#[must_use]
pub fn lint_flags(triple: &str) -> &'static [&'static str] {
    if triple.starts_with("wasm32") {
        &[
            "-A",
            "clippy::all",
            "-A",
            "unfulfilled_lint_expectations",
            "-D",
            "clippy::disallowed_methods",
        ]
    } else {
        &[
            "-A",
            "clippy::all",
            "-D",
            "clippy::disallowed_methods",
            "-D",
            "clippy::disallowed_types",
            "-D",
            "unfulfilled_lint_expectations",
        ]
    }
}

/// The workspace member names, from `targo metadata --no-deps` (the NAME,
fn member_names(ctx: &Ctx) -> Result<BTreeSet<String>, String> {
    let out = aterm_uds::spawnfd::command(&ctx.tools.targo)
        .args([
            "metadata",
            "--no-deps",
            "--locked",
            "--offline",
            "--format-version",
            "1",
        ])
        .current_dir(&ctx.root)
        .env("PATH", &ctx.path_env)
        .output_owned()
        .map_err(|e| format!("could not run `targo metadata`: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "`targo metadata --no-deps` failed: {}",
            String::from_utf8_lossy(&out.stderr).trim_end()
        ));
    }
    parse_member_names(&String::from_utf8_lossy(&out.stdout))
}

/// The package names in `cargo metadata --no-deps` JSON.
///
/// # Errors
/// Not JSON, or no `packages`.
pub fn parse_member_names(json: &str) -> Result<BTreeSet<String>, String> {
    let value = aterm_json::from_str::<aterm_json::Value>(json)
        .map_err(|e| format!("`targo metadata --no-deps` did not print JSON: {e}"))?;
    let names: BTreeSet<String> = value
        .get("packages")
        .and_then(aterm_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|p| p.get("name").and_then(aterm_json::Value::as_str))
        .map(str::to_string)
        .collect();
    if names.is_empty() {
        return Err("`targo metadata --no-deps` listed no workspace members".to_string());
    }
    Ok(names)
}

/// The members this cell's graph carries, and the ones `--exclude` drops.
#[must_use]
pub fn test_selection(
    members: &BTreeSet<String>,
    graph: &BTreeSet<String>,
    patched: &BTreeSet<String>,
) -> (Vec<String>, Vec<String>) {
    let selected = members.intersection(graph).cloned().collect();
    let excluded = members
        .difference(graph)
        .filter(|n| !patched.contains(*n))
        .cloned()
        .collect();
    (selected, excluded)
}

/// The packages the root manifest's `[patch.crates-io]` rows replace.
///
/// # Errors
/// The manifest could not be read ("could not look" is not "there are none").
pub fn patched_crates(root: &Path) -> Result<BTreeSet<String>, String> {
    let manifest = root.join("Cargo.toml");
    let text = std::fs::read_to_string(&manifest)
        .map_err(|e| format!("could not read {} ({e})", manifest.display()))?;
    let mut out = BTreeSet::new();
    let mut inside = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            inside =
                line.starts_with("[patch.crates-io]") || line.starts_with("[patch.\"crates-io\"]");
            continue;
        }
        if !inside || line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, spec)) = line.split_once('=') {
            let name = renamed_package(spec).unwrap_or_else(|| key.trim().trim_matches('"').trim());
            if !name.is_empty() {
                out.insert(name.to_string());
            }
        }
    }
    Ok(out)
}

/// The `package = "…"` of an inline patch spec, if it has one.
#[must_use]
pub fn renamed_package(spec: &str) -> Option<&str> {
    spec.match_indices("package").find_map(|(at, word)| {
        let value = spec[at + word.len()..]
            .trim_start()
            .strip_prefix('=')?
            .trim_start()
            .strip_prefix('"')?;
        value.split('"').next().filter(|p| !p.is_empty())
    })
}

/// What cargo's JSON stream said.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct CargoRead {
    pub checked: BTreeSet<String>,
    pub errors: Vec<(String, String)>,
    pub warnings: Vec<(String, String)>,
}

/// Whether a cell's rustc warnings ([`is_cell_warning`]) are findings.
#[must_use]
pub fn judges_warnings(triple: &str) -> bool {
    let os = |t: &str| t.split_once('-').map(|(_, os)| os.to_string());
    let Some(own) = os(triple) else {
        return false;
    };
    WARNINGS_JUDGED_OFF_FLEET.contains(&own.as_str())
        || FLEET_HOST_TRIPLES
            .iter()
            .any(|host| os(host).as_deref() == Some(own.as_str()))
}

/// The OSes (a triple past its architecture) no fleet machine runs whose cells still judge warnings ([`judges_warnings`]).
pub const WARNINGS_JUDGED_OFF_FLEET: [&str; 1] = ["pc-windows-msvc"];

/// Whether a warning's lint code makes a cell red: any lint rustc itself raises.
#[must_use]
pub fn is_cell_warning(code: Option<&str>) -> bool {
    code.is_some_and(|c| {
        !c.starts_with("clippy::") && !matches!(c, "unknown_lints" | "renamed_and_removed_lints")
    })
}

/// Read the JSON lines of `output` (cargo's other lines are skipped).
#[must_use]
pub fn read_cargo_json(
    output: &str,
    triple: &str,
    graph: &BTreeSet<String>,
    tests_only: bool,
) -> CargoRead {
    let mut read = CargoRead::default();
    let under = format!("/{triple}/");
    for line in output.lines().filter(|l| l.starts_with('{')) {
        let Ok(value) = aterm_json::from_str::<aterm_json::Value>(line) else {
            continue;
        };
        let pkg = value
            .get("package_id")
            .and_then(aterm_json::Value::as_str)
            .map_or("<unknown>", package_id_name)
            .to_string();
        match value.get("reason").and_then(aterm_json::Value::as_str) {
            Some("compiler-artifact") => {
                let is_test = value
                    .get("profile")
                    .and_then(|p| p.get("test"))
                    .and_then(aterm_json::Value::as_bool)
                    .unwrap_or(false);
                let for_triple = value
                    .get("filenames")
                    .and_then(aterm_json::Value::as_array)
                    .is_some_and(|a| {
                        a.iter()
                            .filter_map(aterm_json::Value::as_str)
                            .any(|f| f.contains(&under))
                    });
                if (is_test || !tests_only) && for_triple && graph.contains(&pkg) {
                    read.checked.insert(pkg);
                }
            }
            Some("compiler-message") => {
                let msg = value.get("message");
                let rendered = || {
                    msg.and_then(|m| m.get("rendered"))
                        .and_then(aterm_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                };
                match msg
                    .and_then(|m| m.get("level"))
                    .and_then(aterm_json::Value::as_str)
                {
                    Some("error") => read.errors.push((pkg, rendered())),
                    Some("warning") => {
                        let code = msg
                            .and_then(|m| m.get("code"))
                            .and_then(|c| c.get("code"))
                            .and_then(aterm_json::Value::as_str);
                        let warning = (pkg, rendered());
                        if is_cell_warning(code) && !read.warnings.contains(&warning) {
                            read.warnings.push(warning);
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    read
}

/// The `error` lines cargo printed itself, at column zero (a failing build script's output is re-emitted indented; `could not compile` summarises diagnostics the JSON already carried).
#[must_use]
pub fn cargo_level_errors(output: &str) -> Vec<String> {
    output
        .lines()
        .filter(|l| l.starts_with("error") && !l.starts_with("error: could not compile"))
        .map(str::to_string)
        .collect()
}

/// The package name inside a cargo `package_id`.
#[must_use]
pub fn package_id_name(id: &str) -> &str {
    let Some((url, frag)) = id.rsplit_once('#') else {
        return id.split_whitespace().next().unwrap_or(id);
    };
    if let Some((name, _)) = frag.split_once('@') {
        return name;
    }
    url.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(url)
        .trim_end_matches(".git")
}

/// Is `dir` inside `root`, both canonicalised (`/tmp` and `/private/tmp`
fn not_owed_note(proc_macros: &BTreeSet<String>, host_deps: &BTreeSet<String>) -> String {
    let mut kinds = Vec::new();
    if !proc_macros.is_empty() {
        kinds.push(format!("{} proc macro(s)", proc_macros.len()));
    }
    if !host_deps.is_empty() {
        kinds.push(format!(
            "{} crate(s) only a proc macro reaches",
            host_deps.len()
        ));
    }
    if kinds.is_empty() {
        return String::new();
    }
    let names: Vec<&str> = proc_macros.union(host_deps).map(String::as_str).collect();
    format!(" ({} not owed: {})", kinds.join(" and "), names.join(", "))
}

/// The cell's packages cargo compiles for the HOST: every proc macro, and every package only proc macros reach.
pub fn host_side(graph: &Graph, is_proc_macro: impl Fn(&PkgId) -> bool) -> BTreeSet<PkgId> {
    let mut target = BTreeSet::from([graph.root.clone()]);
    let mut stack = vec![graph.root.clone()];
    while let Some(u) = stack.pop() {
        for v in graph.edges.get(&u).into_iter().flatten() {
            if !is_proc_macro(v) && target.insert(v.clone()) {
                stack.push(v.clone());
            }
        }
    }
    graph.nodes.difference(&target).cloned().collect()
}

fn path_is_inside(dir: &Path, root: &Path) -> bool {
    match (dir.canonicalize(), root.canonicalize()) {
        (Ok(d), Ok(r)) => d.starts_with(r),
        _ => dir.starts_with(root),
    }
}

/// Does the manifest in `dir` declare `proc-macro = true` under `[lib]`?
fn is_proc_macro(dir: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(dir.join("Cargo.toml")) else {
        return false;
    };
    let mut in_lib = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_lib = line == "[lib]";
        } else if in_lib && line.replace(' ', "").starts_with("proc-macro=true") {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> BTreeSet<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn a_policy_row_is_read_strictly() {
        let row = |cols: &str| format!("# a comment\n\n{cols}\n");
        let shims = parse_policy(&row(
            "cshim\tring@0.17.14\tx86_64-pc-windows-msvc\tring_core\twhy",
        ))
        .expect("a good row");
        assert_eq!(
            shims,
            [Shim {
                package: "ring".into(),
                version: "0.17.14".into(),
                triple: "x86_64-pc-windows-msvc".into(),
                links: "ring_core".into(),
            }]
        );
        assert_eq!(
            shims[0].config(),
            "target.x86_64-pc-windows-msvc.ring_core.rustc-link-lib=[\"ring_core\"]"
        );
        for bad in [
            "excuse\tring@1\tx86_64-pc-windows-msvc\tr\twhy",
            "cshim\tring@1\tx86_64-pc-windows-msvc",
            "cshim\tring\tx86_64-pc-windows-msvc\tr\twhy",
            "cshim\tring@1\t*\tr\twhy",
            "cshim\tring@1\twasm32-unknown-unknown\tr\twhy",
        ] {
            assert!(parse_policy(&row(bad)).is_err(), "{bad}");
        }
    }

    /// No `--cell` is the foreign cells; a name picks that cell, hosted or
    #[test]
    fn a_run_checks_the_foreign_cells_or_each_cell_named() {
        assert_eq!(selected(&[]).expect("default"), foreign_cells());
        let named = selected(&["linux".to_string(), "win".to_string()]).expect("named");
        let names: Vec<&str> = named.iter().map(|c| c.name.as_str()).collect();
        assert!(
            names.contains(&"linux") && names.contains(&"win"),
            "{names:?}"
        );
        assert_eq!(named.len(), 2);
        assert!(selected(&["plan9".to_string()]).is_err());
    }

    #[test]
    fn the_host_triple_is_read_off_the_compilers_version() {
        let vv = "rustc 1.93.0 (trust)\nbinary: rustc\nhost: aarch64-apple-darwin\nrelease: 1\n";
        assert_eq!(host_of(vv), Some("aarch64-apple-darwin"));
        assert_eq!(host_of("rustc 1.93.0\nhost: \n"), None);
        assert_eq!(host_of(""), None);
    }

    /// A row's version must be one the cell's graph carries.
    #[test]
    fn a_shim_whose_version_left_the_graph_is_dead() {
        let shim = Shim {
            package: "ring".into(),
            version: "0.17.14".into(),
            triple: "x86_64-pc-windows-msvc".into(),
            links: "ring_core".into(),
        };
        let graph = |versions: &[&str]| -> BTreeSet<PkgId> {
            versions
                .iter()
                .map(|v| PkgId {
                    name: "ring".into(),
                    version: (*v).into(),
                })
                .collect()
        };
        let none = BTreeSet::new();
        assert_eq!(dead_shim(&shim, &graph(&["0.17.14"]), &none), None);
        assert_eq!(
            dead_shim(&shim, &none, &graph(&["0.17.14"])),
            None,
            "a row the test targets need is live"
        );
        let why = dead_shim(&shim, &graph(&["0.17.15"]), &none).expect("dead");
        assert!(why.contains("carry 0.17.15"), "{why}");
        assert!(
            dead_shim(&shim, &none, &none)
                .expect("dead")
                .contains("no such package")
        );
    }

    /// A test-only C oracle is live in the test-target pass alone; a stale
    #[test]
    fn a_test_only_c_oracle_is_shimmed_only_in_the_test_target_pass() {
        let normal = aterm_forge::resolve::parse_tree(
            "0app v1.0.0 (/fixture/app)\n1decoder v1.0.0 (/fixture/decoder)\n\
             1normal-c v2.0.0\n",
        )
        .expect("normal dependency tree")
        .0;
        let tests = aterm_forge::resolve::parse_tree(
            "0decoder v1.0.0 (/fixture/decoder)\n1oracle v1.0.0\n2oracle-sys v3.0.0\n\
             1normal-c v2.0.0\n",
        )
        .expect("decoder's test dependency tree")
        .0;
        let row = |package: &str, version: &str| Shim {
            package: package.into(),
            version: version.into(),
            triple: "x86_64-unknown-linux-gnu".into(),
            links: "c".into(),
        };
        assert_eq!(
            shim_lanes(&row("normal-c", "2.0.0"), &normal.nodes, &tests.nodes),
            (true, true)
        );
        assert_eq!(
            shim_lanes(&row("oracle-sys", "3.0.0"), &normal.nodes, &tests.nodes),
            (false, true),
            "an oracle is needed by test targets without entering the shipped graph"
        );
        for absent in [row("oracle-sys", "2.0.0"), row("unselected-sys", "3.0.0")] {
            assert_eq!(
                shim_lanes(&absent, &normal.nodes, &tests.nodes),
                (false, false),
                "a different version or an unselected member's oracle does not make a row live"
            );
        }
    }

    /// The test-target forest of a real (private, compiler-free) workspace
    #[test]
    fn the_test_target_forest_has_only_selected_members_dev_and_target_dependencies() {
        let dir = aterm_tempfile::tempdir().expect("private resolver fixture");
        let root = dir.path();
        let packages = [
            (
                "app",
                "[dependencies]\ndecoder={path='../decoder'}\npm={path='../pm'}\n\
                 [build-dependencies]\nbuild-only={path='../build-only'}\n",
            ),
            ("decoder", "[dev-dependencies]\noracle={path='../oracle'}\n"),
            (
                "oracle",
                "[dev-dependencies]\nignored-sys={path='../ignored-sys'}\n",
            ),
            (
                "ignored",
                "[dev-dependencies]\nignored-sys={path='../ignored-sys'}\n",
            ),
            ("ignored-sys", ""),
            ("build-only", ""),
            (
                "pm",
                "[lib]\nproc-macro=true\n[dependencies]\nhost-only={path='../host-only'}\n",
            ),
            ("host-only", ""),
        ];
        let members: BTreeSet<String> = packages
            .iter()
            .map(|(name, _)| (*name).to_string())
            .filter(|name| name != "host-only")
            .collect();
        std::fs::write(
            root.join("Cargo.toml"),
            format!(
                "[workspace]\nresolver='2'\nmembers={:?}\nexclude=['host-only']\n",
                members.iter().collect::<Vec<_>>()
            ),
        )
        .expect("workspace manifest");
        for (name, deps) in packages {
            let package = root.join(name);
            std::fs::create_dir_all(package.join("src")).expect("fixture source dir");
            std::fs::write(package.join("src/lib.rs"), "").expect("empty library");
            std::fs::write(
                package.join("Cargo.toml"),
                format!("[package]\nname='{name}'\nversion='1.0.0'\nedition='2021'\n{deps}"),
            )
            .expect("package manifest");
        }
        std::fs::write(
            root.join("Cargo.lock"),
            "version=4\n\
             [[package]]\nname='app'\nversion='1.0.0'\ndependencies=['build-only','decoder','pm']\n\
             [[package]]\nname='build-only'\nversion='1.0.0'\n\
             [[package]]\nname='decoder'\nversion='1.0.0'\ndependencies=['oracle']\n\
             [[package]]\nname='host-only'\nversion='1.0.0'\n\
             [[package]]\nname='ignored'\nversion='1.0.0'\ndependencies=['ignored-sys']\n\
             [[package]]\nname='ignored-sys'\nversion='1.0.0'\n\
             [[package]]\nname='oracle'\nversion='1.0.0'\ndependencies=['ignored-sys']\n\
             [[package]]\nname='pm'\nversion='1.0.0'\ndependencies=['host-only']\n",
        )
        .expect("locked local dependency graph");
        let normal: BTreeSet<String> = ["app", "decoder", "pm", "host-only"]
            .into_iter()
            .map(str::to_string)
            .collect();
        let (selected, excluded) = test_selection(&members, &normal, &BTreeSet::new());
        assert_eq!(selected, ["app", "decoder", "pm"]);
        assert_eq!(excluded, ["build-only", "ignored", "ignored-sys", "oracle"]);
        let targo = PathBuf::from(env!("CARGO"));
        let path = std::env::var_os("PATH").unwrap_or_default();
        let forest = test_tree(&targo, &path, root, "x86_64-pc-windows-msvc", &excluded)
            .expect("resolve the actual test member selection without compiling");
        let roots: BTreeSet<String> = members
            .difference(&excluded.iter().cloned().collect())
            .cloned()
            .collect();
        let nodes = parse_test_forest(&forest, &roots).expect("the selected members' forest");
        let names: Vec<_> = nodes.iter().map(|id| id.name.as_str()).collect();
        assert_eq!(names, ["app", "decoder", "oracle"]);
        let one: BTreeSet<String> = ["app".to_string()].into_iter().collect();
        assert!(parse_test_forest("", &one).is_err());
        assert!(parse_test_forest("0other v1.0.0\n", &one).is_err());
    }

    /// A member a selected proc macro depends on is printed as two roots, its
    #[test]
    fn a_member_a_proc_macro_uses_is_read_as_its_target_copy() {
        let dir = aterm_tempfile::tempdir().expect("private resolver fixture");
        let root = dir.path();
        let packages = [
            (
                "app",
                "[dependencies]\npm={path='../pm'}\nshared={path='../shared'}\n",
            ),
            (
                "pm",
                "[lib]\nproc-macro=true\n[dependencies]\nshared={path='../shared'}\n",
            ),
            (
                "shared",
                "[target.'cfg(windows)'.dependencies]\nwin-only={path='../win-only'}\n\
                 [target.'cfg(not(windows))'.dependencies]\nunix-only={path='../unix-only'}\n",
            ),
            ("win-only", ""),
            ("unix-only", ""),
        ];
        let members = names(&["app", "pm", "shared"]);
        std::fs::write(
            root.join("Cargo.toml"),
            format!(
                "[workspace]\nresolver='2'\nmembers={:?}\nexclude=['win-only','unix-only']\n",
                members.iter().collect::<Vec<_>>()
            ),
        )
        .expect("workspace manifest");
        for (name, deps) in packages {
            let package = root.join(name);
            std::fs::create_dir_all(package.join("src")).expect("fixture source dir");
            std::fs::write(package.join("src/lib.rs"), "").expect("empty library");
            std::fs::write(
                package.join("Cargo.toml"),
                format!("[package]\nname='{name}'\nversion='1.0.0'\nedition='2021'\n{deps}"),
            )
            .expect("package manifest");
        }
        std::fs::write(
            root.join("Cargo.lock"),
            "version=4\n\
             [[package]]\nname='app'\nversion='1.0.0'\ndependencies=['pm','shared']\n\
             [[package]]\nname='pm'\nversion='1.0.0'\ndependencies=['shared']\n\
             [[package]]\nname='shared'\nversion='1.0.0'\ndependencies=['unix-only','win-only']\n\
             [[package]]\nname='unix-only'\nversion='1.0.0'\n\
             [[package]]\nname='win-only'\nversion='1.0.0'\n",
        )
        .expect("locked local dependency graph");
        let targo = PathBuf::from(env!("CARGO"));
        let path = std::env::var_os("PATH").unwrap_or_default();
        let forest = test_tree(&targo, &path, root, "x86_64-pc-windows-msvc", &[])
            .expect("resolve the fixture without compiling");
        let nodes = parse_test_forest(&forest, &members)
            .expect("a member's host copy is not a second target root");
        let found: Vec<_> = nodes.iter().map(|id| id.name.as_str()).collect();
        assert_eq!(found, ["app", "shared", "win-only"]);

        let err = parse_test_forest(
            "0shared v1.0.0\n1win-only v1.0.0\n0shared v1.0.0\n1unix-only v1.0.0\n",
            &names(&["shared"]),
        )
        .expect_err("an unexplained repeat");
        assert!(err.contains("no proc macro"), "{err}");
    }

    #[test]
    fn package_id_names_survive_all_three_id_shapes() {
        assert_eq!(package_id_name("registry+https://x#serde@1.0.228"), "serde");
        assert_eq!(
            package_id_name("path+file:///r/crates/foo#bar@0.1.0"),
            "bar"
        );
        assert_eq!(
            package_id_name("path+file:///r/crates/serde#1.0.228"),
            "serde"
        );
        assert_eq!(
            package_id_name("serde 1.0.228 (registry+https://x)"),
            "serde"
        );
    }

    /// Only an artifact under the triple's own directory counts, a host unit
    #[test]
    fn only_the_triples_own_artifacts_count() {
        let graph = names(&["a", "b", "c"]);
        let out = [
            r#"{"reason":"compiler-artifact","package_id":"path+file:///r/a#a@1","profile":{"test":false},"filenames":["/t/x86_64-pc-windows-msvc/debug/liba.rmeta"]}"#,
            r#"{"reason":"compiler-artifact","package_id":"path+file:///r/b#b@1","profile":{"test":false},"filenames":["/t/debug/deps/libb.dylib"]}"#,
            r#"{"reason":"compiler-artifact","package_id":"path+file:///r/c#c@1","profile":{"test":true},"filenames":["/t/x86_64-pc-windows-msvc/debug/c-1"]}"#,
            r#"{"reason":"compiler-message","package_id":"path+file:///r/a#a@1","message":{"level":"error","rendered":"error[E0425]: x"}}"#,
            "error: failed to run custom build command for `zstd-sys`",
            "error: could not compile `a`",
            "   Compiling a v1",
        ]
        .join("\n");
        let all = read_cargo_json(&out, "x86_64-pc-windows-msvc", &graph, false);
        assert_eq!(all.checked, names(&["a", "c"]));
        assert_eq!(
            all.errors,
            [("a".to_string(), "error[E0425]: x".to_string())]
        );
        let tests = read_cargo_json(&out, "x86_64-pc-windows-msvc", &graph, true);
        assert_eq!(tests.checked, names(&["c"]));
        assert_eq!(
            cargo_level_errors(&out),
            ["error: failed to run custom build command for `zstd-sys`"]
        );
    }

    /// rustc's own warnings are the cell's: an item only another OS calls
    #[test]
    fn a_rustc_warning_is_a_cell_finding_and_a_clippy_one_is_the_hosts() {
        let graph = names(&["gui"]);
        let warning = |code: &str, rendered: &str| {
            format!(
                r#"{{"reason":"compiler-message","package_id":"path+file:///r/gui#gui@1","message":{{"level":"warning","code":{code},"rendered":"{rendered}"}}}}"#
            )
        };
        let dead = "warning: function `launchctl_list` is never used";
        let out = [
            warning(r#"{"code":"dead_code","explanation":null}"#, dead),
            warning(r#"{"code":"dead_code","explanation":null}"#, dead),
            warning(
                r#"{"code":"unused_imports","explanation":null}"#,
                "warning: unused import: `HashSet`",
            ),
            warning(
                r#"{"code":"clippy::duration_suboptimal_units","explanation":null}"#,
                "warning: constructing a `Duration` using a smaller unit",
            ),
            warning(
                r#"{"code":"unknown_lints","explanation":null}"#,
                "warning: unknown lint: `newer_lint`",
            ),
            warning("null", "warning: 3 warnings emitted"),
        ]
        .join("\n");
        let read = read_cargo_json(&out, "aarch64-unknown-linux-gnu", &graph, false);
        assert!(read.errors.is_empty());
        assert_eq!(
            read.warnings,
            [
                ("gui".to_string(), dead.to_string()),
                (
                    "gui".to_string(),
                    "warning: unused import: `HashSet`".to_string()
                ),
            ]
        );
        assert!(is_cell_warning(Some("unused_variables")));
        for (triple, judged) in [
            ("aarch64-unknown-linux-gnu", true),
            ("x86_64-unknown-linux-gnu", true),
            ("aarch64-apple-darwin", true),
            ("x86_64-apple-darwin", true),
            ("x86_64-pc-windows-msvc", true),
            ("aarch64-pc-windows-msvc", true),
            ("x86_64-pc-windows-gnu", false),
            ("wasm32-unknown-unknown", false),
        ] {
            assert_eq!(judges_warnings(triple), judged, "{triple}");
        }
        assert!(!is_cell_warning(Some("clippy::single_match")));
        assert!(!is_cell_warning(Some("renamed_and_removed_lints")));
        assert!(!is_cell_warning(None));
        for (tests, status) in [(false, "WARNING(2)"), (true, "TEST-TARGET-WARNING(2)")] {
            let run = exec::Run {
                ok: true,
                output: out.clone(),
                code: Some(0),
                spawn_error: None,
            };
            let mut red = Red::default();
            read_cargo_run(&run, "aarch64-unknown-linux-gnu", &graph, tests, &mut red)
                .expect("the compiler ran");
            assert_eq!(red.status.as_deref(), Some(status));
            assert_eq!(red.detail.len(), 2);
            assert!(red.detail[0].contains("launchctl_list"), "{:?}", red.detail);
        }
        let run = exec::Run {
            ok: true,
            output: out.clone(),
            code: Some(0),
            spawn_error: None,
        };
        let mut red = Red::default();
        read_cargo_run(&run, "x86_64-pc-windows-msvc", &graph, false, &mut red)
            .expect("the compiler ran");
        assert_eq!(
            red.status.as_deref(),
            Some("WARNING(2)"),
            "{:?}",
            red.detail
        );
        let mut red = Red::default();
        read_cargo_run(&run, "wasm32-unknown-unknown", &graph, false, &mut red)
            .expect("the compiler ran");
        assert!(red.status.is_none(), "{:?}", red.detail);
        let clippy_only = warning(
            r#"{"code":"clippy::duration_suboptimal_units","explanation":null}"#,
            "warning: constructing a `Duration` using a smaller unit",
        );
        let run = exec::Run {
            ok: true,
            output: clippy_only,
            code: Some(0),
            spawn_error: None,
        };
        let mut red = Red::default();
        read_cargo_run(&run, "aarch64-unknown-linux-gnu", &graph, false, &mut red)
            .expect("the compiler ran");
        assert!(red.status.is_none(), "{:?}", red.detail);
    }

    /// Both passes can leave every owed artifact before cargo itself fails.
    #[test]
    fn complete_cell_artifacts_do_not_make_a_failed_child_green() {
        let artifact = r#"{"reason":"compiler-artifact","package_id":"path+file:///r/app#app@1","profile":{"test":true},"filenames":["/t/x86_64-pc-windows-msvc/debug/app"]}"#;
        let graph = names(&["app"]);
        let run = |code| exec::Run {
            ok: code == Some(0),
            output: artifact.to_string(),
            code,
            spawn_error: None,
        };
        for tests in [false, true] {
            let mut red = Red::default();
            let read = read_cargo_run(
                &run(Some(0)),
                "x86_64-pc-windows-msvc",
                &graph,
                tests,
                &mut red,
            )
            .expect("a successful compiler ran");
            assert_eq!(read.checked, graph);
            assert!(red.status.is_none());
            for code in [Some(17), None] {
                let mut red = Red::default();
                let read = read_cargo_run(
                    &run(code),
                    "x86_64-pc-windows-msvc",
                    &graph,
                    tests,
                    &mut red,
                )
                .expect("the compiler answered a real failure");
                assert_eq!(read.checked, graph, "the fixture has complete coverage");
                assert!(
                    red.status.is_some(),
                    "all artifacts concealed the child failure: tests={tests}, code={code:?}"
                );
            }
        }
    }

    #[test]
    fn a_failed_cell_keeps_its_diagnostics_and_machine_refusals_distinct() {
        let graph = names(&["app"]);
        for tests in [false, true] {
            for diagnostic in [
                r#"{"reason":"compiler-message","package_id":"path+file:///r/app#app@1","message":{"level":"error","rendered":"error[E0425]: missing"}}"#,
                "error: no matching package named `missing` found",
            ] {
                let run = exec::Run {
                    ok: false,
                    output: diagnostic.to_string(),
                    code: Some(101),
                    spawn_error: None,
                };
                let mut red = Red::default();
                read_cargo_run(&run, "x86_64-pc-windows-msvc", &graph, tests, &mut red)
                    .expect("a compiler or dependency finding, not a machine refusal");
                assert!(red.status.is_some());
                assert_eq!(red.detail.len(), 1, "keep the actual compiler diagnostic");
                assert!(red.detail[0].contains("missing"));
            }
            for (spawn_error, output) in [
                (Some("compiler could not start".to_string()), String::new()),
                (None, exec::ENOSPC_SENTENCE.to_string()),
            ] {
                let run = exec::Run {
                    ok: false,
                    output,
                    code: None,
                    spawn_error,
                };
                let mut red = Red::default();
                assert!(matches!(
                    read_cargo_run(&run, "x86_64-pc-windows-msvc", &graph, tests, &mut red),
                    Err(CellOutcome::CouldNotRun { .. })
                ));
                assert!(red.status.is_none());
            }
        }
    }

    #[test]
    fn member_names_come_out_of_cargo_metadata() {
        let json = r#"{"packages":[{"name":"aterm"},{"name":"arrayvec"}],"version":1}"#;
        assert_eq!(
            parse_member_names(json).expect("names"),
            names(&["aterm", "arrayvec"])
        );
        assert!(parse_member_names("not json").is_err());
        assert!(parse_member_names(r#"{"packages":[]}"#).is_err());
    }

    /// The members the graph carries are selected; the rest are excluded,
    #[test]
    fn the_test_pass_selects_the_graph_and_never_excludes_a_patch() {
        let members = names(&["a", "b", "arrayvec", "z"]);
        let graph = names(&["a", "b", "serde"]);
        let (selected, excluded) = test_selection(&members, &graph, &names(&["arrayvec"]));
        assert_eq!(selected, ["a", "b"]);
        assert_eq!(excluded, ["z"]);
    }

    /// A crate only a proc macro reaches compiles for the host with it and is
    #[test]
    fn a_crate_only_proc_macros_reach_is_host_side() {
        let id = |n: &str| PkgId::new(n, "1.0.0");
        let mut g = Graph {
            root: id("root"),
            ..Graph::default()
        };
        for (from, to) in [
            ("root", "lib"),
            ("lib", "pm"),
            ("pm", "only_pm"),
            ("pm", "shared"),
            ("root", "shared"),
        ] {
            g.nodes.insert(id(from));
            g.nodes.insert(id(to));
            g.edges.entry(id(from)).or_default().insert(id(to));
        }
        let host = super::host_side(&g, |p| p.name == "pm");
        assert_eq!(
            host.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            ["only_pm", "pm"]
        );
        assert!(super::host_side(&g, |_| false).is_empty());
    }

    #[test]
    fn the_patch_table_is_read_from_the_root_manifest() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("atv-cells-")
            .tempdir()
            .expect("scratch");
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[workspace]\n[patch.crates-io]\n# x\narrayvec = { path = \"a\" }\n\"libc\" = { path = \"l\" }\n\
             windows-sys-052 = { package = \"windows-sys\", path = \"w\" }\n[profile.dev]\nopt = 1\n",
        )
        .expect("write");
        assert_eq!(
            patched_crates(dir.path()).expect("read"),
            names(&["arrayvec", "libc", "windows-sys"])
        );
        assert!(patched_crates(&dir.path().join("absent")).is_err());
        assert_eq!(
            renamed_package(r#" { path = "crates/package-x", package = "real" }"#),
            Some("real")
        );
        assert_eq!(renamed_package(r#" { path = "crates/x" }"#), None);
    }

    /// A cell's cargo builds in its own directory of the cells lane, capped at
    #[test]
    fn a_cells_cargo_runs_in_the_cells_lane_under_its_job_cap() {
        let env_of = |caller: Option<&str>| {
            let ctx = Ctx::new(
                PathBuf::from("/repo"),
                crate::Scope::workspace(),
                crate::EnvSnapshot {
                    cargo_build_jobs: caller.map(Into::into),
                    ..crate::EnvSnapshot::default()
                },
                PathBuf::from("/tmp"),
            );
            let toolchain = rustup::Toolchain {
                name: "stable".into(),
                cargo: PathBuf::from("/r/bin/cargo"),
                rustc: PathBuf::from("/r/bin/rustc"),
                clippy: true,
            };
            let target = lane_dir(&ctx, Lane::CellsTarget)
                .expect("a side lane")
                .join("win");
            let cargo = CellCargo {
                ctx: &ctx,
                triple: "x86_64-pc-windows-msvc",
                toolchain: &toolchain,
                cwd: Path::new("/tmp/cells-win"),
                target_dir: &target,
                shim_args: &[],
            };
            let cmd = cargo.all_targets(&[]);
            let get = |k: &str| {
                cmd.envs
                    .iter()
                    .find(|(name, _)| name == k)
                    .map(|(_, v)| v.to_string_lossy().into_owned())
            };
            (get("CARGO_TARGET_DIR"), get("CARGO_BUILD_JOBS"))
        };
        let dir = Some("/repo/target/verify/cells.noindex/win".to_string());
        assert_eq!(env_of(None), (dir.clone(), Some("4".to_string())));
        assert_eq!(env_of(Some("2")), (dir.clone(), Some("2".to_string())));
        assert_eq!(env_of(Some("16")), (dir, Some("4".to_string())));
    }

    /// Either Windows cell brings both windows-sys oracles, once each; a run
    #[test]
    fn a_windows_cell_brings_both_windows_sys_oracles_built_in_the_cells_lane() {
        let all = aterm_forge::resolve::default_cells();
        let pick = |names: &[&str]| {
            aterm_forge::resolve::select(
                &all,
                &names.iter().map(|n| (*n).to_string()).collect::<Vec<_>>(),
            )
            .expect("forge cells")
        };
        assert!(runs_win32_oracles(&pick(&["win"])));
        assert!(runs_win32_oracles(&pick(&["win-arm"])));
        assert!(runs_win32_oracles(&pick(&["linux-arm", "win", "win-arm"])));
        assert!(!runs_win32_oracles(&pick(&[
            "linux-arm",
            "wasm-cpu",
            "mac-x64"
        ])));
        assert!(runs_win32_oracles(&foreign_cells()), "the default run");
        assert_eq!(
            WIN32_ORACLES.map(|(verb, _)| verb),
            ["oracle", "oracle-052"]
        );

        let ctx = Ctx::new(
            PathBuf::from("/repo"),
            crate::Scope::workspace(),
            crate::EnvSnapshot::default(),
            PathBuf::from("/tmp"),
        );
        let cmd = win32_oracle_cmd(&ctx, "oracle-052").expect("absolute paths");
        let argv = cmd.argv();
        assert_eq!(
            argv[1..],
            [
                "--unverified",
                "run",
                "-q",
                "-p",
                "aterm-win32-oracle",
                "--target-dir",
                "/repo/target/verify/cells.noindex/win32-oracle-bin",
                "--",
                "oracle-052",
                "--root",
                "/repo",
            ]
        );
        assert_eq!(cmd.cwd.as_deref(), Some(Path::new("/repo")));
        assert!(
            !cmd.envs.iter().any(|(k, _)| k == "CARGO_TARGET_DIR"),
            "the oracle's own Windows builds keep their per-checkout directories"
        );
    }

    /// An oracle that decided nothing says why on its last `could not run`
    #[test]
    fn a_win32_oracle_that_could_not_run_names_its_reason() {
        assert_eq!(
            win32_oracle_not_run_reason(
                "== x86_64-pc-windows-msvc\nwin32 oracle could not run: no cached \
                 windows-sys0.61.2; use --upstream\n"
            )
            .as_deref(),
            Some("no cached windows-sys0.61.2; use --upstream")
        );
        assert_eq!(
            win32_oracle_not_run_reason(
                "windows-sys 0.52 oracle could not run: stock cargo failed\n"
            )
            .as_deref(),
            Some("stock cargo failed")
        );
        assert_eq!(
            win32_oracle_not_run_reason("  FAIL plant-dll was not caught\nwin32 oracle: FAIL\n"),
            None
        );
    }

    #[test]
    fn the_lint_denies_the_bans_and_the_browser_cells_only_the_clock() {
        assert!(lint_flags("x86_64-pc-windows-msvc").contains(&"clippy::disallowed_types"));
        let wasm = lint_flags("wasm32-unknown-unknown");
        assert!(wasm.contains(&"clippy::disallowed_methods"));
        assert!(!wasm.contains(&"clippy::disallowed_types"));
    }
}
