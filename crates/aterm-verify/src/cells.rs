// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `verify cells`: cross-cell lint and type-check of every forge cell but this machine's own
//! (Windows, Linux, both Mac slices and wasm32, less the host's).
//!
//! STOCK EXCEPTION: the Trust sysroot carries only the host std, so every cell compiles and
//! lints on the one stock toolchain the checkout pins ([`rustup::stock_pin`]), named in its
//! row, driven by path from a neutral cwd so neither this repo's Trust pin nor its Trust-only
//! cargo config reaches it. A machine whose pin lacks a cell's std or clippy has that cell
//! COULD NOT RUN, naming `verify provision`: no other toolchain stands in, so one tree's verdict
//! is the same on every Mac whatever rustup's floating `stable` is there.
use aterm_uds::output::CommandOutput as _;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Instant;

use aterm_forge::model::{Cell, Graph, PkgId};

use crate::Ctx;

#[cfg(test)]
#[path = "cells_unitgraph_tests.rs"]
mod unit_graph_tests;
#[path = "cells_units.rs"]
mod units;
use crate::exec::{self, Cmd};
use crate::ladder::{Outcome, Report, Severity};
use crate::plan::{Lane, lane_dir};
use crate::rustup;
use crate::stages::lane_jobs;

/// The build-script shims a cell may apply.
pub const POLICY: &str = "tools/cross-cell-gate.tsv";

/// The triples some machine in this fleet runs natively: their OSes' cells are red on any
/// rustc warning ([`judges_warnings`]).
pub const FLEET_HOST_TRIPLES: [&str; 3] = [
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-unknown-linux-gnu",
];

/// The clippy config the browser cells lint with: std's clock panics on
/// wasm32-unknown-unknown, which no host lint can see.
const WASM_CLIPPY_CONF: &str = "crates/aterm-time/wasm32";

/// The cells this check compiles by default: forge's own list, less the cell of `host`, this
/// machine's triple, which its own build, test and lint judge. A triple another machine could
/// host is checked here too: the release ships the Intel Mac slice and x86_64 Linux, and no
/// other check lints either.
#[must_use]
pub fn foreign_cells(host: &str) -> Vec<Cell> {
    aterm_forge::resolve::default_cells()
        .into_iter()
        .filter(|c| c.triple != host)
        .collect()
}

/// The cells a pulled run checks ([`crate::checks::pulls_cells`]): of [`foreign_cells`], those
/// of the OSes other than `host`'s, one per OS and root package, the cell of `host`'s
/// architecture where the OS has one. An OS's code differs from this machine's in its `cfg`s,
/// which one cell of it judges; an architecture's (x86_64 Linux and Windows on an Apple-silicon
/// Mac) is left to `verify cells`, which checks every cell. Then one cell of `host`'s own OS
/// ([`lint_only_when_pulled`]): the host's code is compiled with debug assertions off by no other
/// dev check, and a cell's lint pass does ([`CellCargo::lint`]).
#[must_use]
pub fn pulled_cells(host: &str) -> Vec<Cell> {
    let mut kept: Vec<Cell> = other_os_cells(host);
    kept.extend(
        foreign_cells(host)
            .into_iter()
            .find(|cell| lint_only_when_pulled(&cell.triple, host)),
    );
    kept
}

/// Whether a pulled run checks `triple` only for its lint pass: a cell of `host`'s own OS, whose
/// test targets differ from what `host`'s own `test` and `lint` compile only by architecture. It is
/// there for shipped code that calls a seam only debug assertions compile, a compile error no dev
/// check of `host` can see, at a lint pass's cost (docs/measured/release-lint-in-dev-2026-11-03.md).
#[must_use]
pub fn lint_only_when_pulled(triple: &str, host: &str) -> bool {
    triple != host && os_of(triple) == os_of(host)
}

/// Whether this run checks `triple` for its lint pass alone: a pulled run's
/// ([`lint_only_when_pulled`]), on a host it can name. A run naming `cells` checks every target of
/// every cell.
fn lint_only_in_run(
    pulled: bool,
    triple: &str,
    host: impl FnOnce() -> Result<String, String>,
) -> bool {
    pulled && host().is_ok_and(|host| lint_only_when_pulled(triple, &host))
}

/// [`pulled_cells`]' cells of the OSes other than `host`'s.
fn other_os_cells(host: &str) -> Vec<Cell> {
    let mut kept: Vec<Cell> = Vec::new();
    for cell in foreign_cells(host)
        .into_iter()
        .filter(|c| os_of(&c.triple) != os_of(host))
    {
        let same = kept
            .iter_mut()
            .find(|k| os_of(&k.triple) == os_of(&cell.triple) && k.package == cell.package);
        match same {
            None => kept.push(cell),
            Some(k)
                if arch_of(&k.triple) != arch_of(host)
                    && arch_of(&cell.triple) == arch_of(host) =>
            {
                *k = cell;
            }
            Some(_) => {}
        }
    }
    kept
}

/// A triple's architecture: what precedes its first `-`.
fn arch_of(triple: &str) -> Option<&str> {
    triple.split_once('-').map(|(arch, _)| arch)
}

/// A triple's OS: what follows its architecture (`apple-darwin`, `pc-windows-msvc`).
fn os_of(triple: &str) -> Option<&str> {
    triple.split_once('-').map(|(_, os)| os)
}

/// The selected crates a cell compiles: of `own`, its graph's workspace crates, those `scope`
/// selects. A pulled run checks a cell only when there is one.
#[must_use]
pub fn selected_in(own: &BTreeSet<String>, scope: &crate::scope::Scope) -> Vec<String> {
    own.iter()
        .filter(|name| scope.includes_crate(name))
        .cloned()
        .collect()
}

/// Whether a cell compiles only part of its graph: under `-p`, and in a run the selection pulled
/// ([`crate::checks::pulls_cells`]), every cell but the browser's. Whatever names `cells` with no
/// `-p`, and `--all`, compiles the whole graph: no other check does, `ship release` included. A
/// browser cell's roots turn its crates' default features off, so a crate named alone there is a
/// build nothing ships, and it only lints.
#[must_use]
pub fn narrows(triple: &str, scope: &crate::scope::Scope, pulled: bool) -> bool {
    use crate::scope::Scope;
    !triple.starts_with("wasm32")
        && match scope {
            Scope::Workspace => false,
            Scope::Packages(_) => true,
            Scope::Changed(_) => pulled,
        }
}

/// The roots of the target unit graph that reach any of `seeds`, the
/// seeds' own included: a member's dependents on this cell's triple, dev edges included. The change
/// cone is closed over this machine's edges, so it cannot hold a dependent only another OS has
/// (`aterm-gui` on `aterm-x11`, the Windows crates on `aterm-win32`), which only this graph names.
///
#[must_use]
pub fn dependents_in_graph(
    dependencies: &BTreeMap<String, BTreeSet<PkgId>>,
    seeds: &BTreeSet<String>,
) -> BTreeSet<String> {
    dependencies
        .iter()
        .filter(|(_, nodes)| nodes.iter().any(|id| seeds.contains(&id.name)))
        .map(|(root, _)| root.clone())
        .collect()
}

/// Keep an explicit package selection narrow; close a change cone over this cell's complete
/// build inputs, including host tools that are absent from its normal library graph. Only what the
/// change touched ([`crate::scope::Scope::touches`]) is closed over: a crate of the cone that is
/// there for its tests alone holds the change in no build of its dependents.
#[must_use]
pub fn wanted_in_cell(
    normal: &BTreeSet<String>,
    inputs: Option<&BTreeMap<String, BTreeSet<PkgId>>>,
    scope: &crate::scope::Scope,
) -> BTreeSet<String> {
    let mut wanted = selected_in(normal, scope)
        .into_iter()
        .collect::<BTreeSet<_>>();
    if matches!(scope, crate::scope::Scope::Changed(_))
        && let Some(inputs) = inputs
    {
        let seeds = inputs
            .values()
            .flat_map(|nodes| nodes.iter())
            .filter(|id| scope.touches(&id.name))
            .map(|id| id.name.clone())
            .collect();
        wanted.extend(dependents_in_graph(inputs, &seeds));
    }
    wanted
}

/// The crates a [`narrows`] lint names: of `own`, the `wanted` members this cell compiles for its
/// own target. `not_owed` (host code) is built for this machine in the cell's graph, so naming it
/// would compile it for a target it never ships on.
#[must_use]
pub fn narrowed_lint(
    own: &BTreeSet<String>,
    not_owed: &BTreeSet<String>,
    members: &BTreeSet<String>,
    wanted: &BTreeSet<String>,
) -> BTreeSet<String> {
    own.iter()
        .filter(|name| {
            wanted.contains(*name) && members.contains(*name) && !not_owed.contains(*name)
        })
        .cloned()
        .collect()
}

/// A `-p` naming no workspace member, which a cell cannot narrow to, so a green would mean
/// nothing: a typo, a crate's directory where its package has another name, a vendored crate or a
/// dependency.
#[must_use]
pub fn unknown_packages(scope: &crate::scope::Scope, members: &BTreeSet<String>) -> Vec<String> {
    match scope {
        crate::scope::Scope::Packages(named) => named
            .iter()
            .filter(|spec| {
                !crate::scope::spec_name(spec).is_some_and(|name| members.contains(name))
            })
            .cloned()
            .collect(),
        _ => Vec::new(),
    }
}

/// `-p <spec>` for each of `names`, spelled as `scope` names it ([`crate::scope::Scope::spec_of`]).
#[must_use]
pub fn package_args<'a>(
    names: impl IntoIterator<Item = &'a String>,
    scope: &crate::scope::Scope,
) -> Vec<String> {
    names
        .into_iter()
        .flat_map(|name| {
            let spec = scope.spec_of(name).unwrap_or_else(|| name.clone());
            ["-p".to_string(), spec]
        })
        .collect()
}

/// The cells a run checks: each `--cell` named, else [`foreign_cells`] of `host`.
///
/// # Errors
/// A name forge's cell list does not have.
pub fn selected(names: &[String], host: &str) -> Result<Vec<Cell>, String> {
    if names.is_empty() {
        return Ok(foreign_cells(host));
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
pub(crate) fn host_triple(ctx: &Ctx) -> Result<String, String> {
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

/// One `cshim` row: a C-bearing build script replaced, for one triple, by cargo's own
/// override on the command line.
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

/// [`POLICY`], parsed: `cshim <name>@<version> <triple> <links> <why>`, TAB-separated, `#`
/// and blank lines ignored.
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
    /// A narrowed cell ([`narrows`]) or a pulled one with nothing selected to build for its target:
    /// the selection cannot change what it compiles, so nothing was compiled.
    Unselected,
    /// Nothing was compiled: why, and the install that clears it.
    Skipped { why: String, fix: String },
    /// The tree did not hold for the cell: the first cause, and the lines that say what went
    /// wrong.
    Failed { status: String, detail: Vec<String> },
    /// The machine could not answer for the cell (no graph, no toolchain, a child that never
    /// ran): what failed, and how.
    CouldNotRun { status: String, detail: Vec<String> },
}

/// The `cells` check's stage: one row per selected cell, the windows-sys oracles when a
/// Windows cell is selected, then the `cshim` rows nothing could use.
pub fn run(ctx: &Ctx, r: &mut Report) {
    let policy = match crate::read_regular_text(ctx.root.join(POLICY))
        .map_err(|e| format!("cannot read {POLICY}: {e}"))
        .and_then(|t| parse_policy(&t))
    {
        Ok(p) => p,
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
    let cells = if ctx.cells_pulled {
        Ok(pulled_cells(&host))
    } else {
        selected(&ctx.cells, &host)
    };
    let cells = match cells {
        Ok(c) => c,
        Err(why) => {
            r.cannot_run(format!("cells — {why}"));
            return;
        }
    };
    if ctx.cells_pulled {
        r.raw(format!(
            "cells: pulled by the selection, so one cell per other OS and one of this OS for its \
             debug-off lint ({}), each where its graph holds a selected crate; `targo --unverified \
             verify cells` checks every cell",
            cells
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let mut members: Option<Result<Members, String>> = None;
    if let Ok(known) = members.get_or_insert_with(|| member_names(ctx)) {
        let unknown = unknown_packages(&ctx.scope, &known.names);
        if !unknown.is_empty() {
            r.cannot_run(format!(
                "cells — -p {} is no workspace member's package name, so no cell can narrow to it \
                 (a directory can have another name; vendored crates and dependencies are not \
                 members, and `verify cells` with no -p checks them in the whole graph)",
                unknown.join(", ")
            ));
            return;
        }
    }
    let stock = stock_of(ctx);
    for cell in &cells {
        // A cell that failed ends a run whose halt is armed before the next cell starts.
        crate::red_now(ctx, crate::checks::Check::Cells, r);
        let label = format!("cell {} ({})", cell.name, cell.triple);
        let started = Instant::now();
        let outcome = if let Some(why) = cell_ended(ctx) {
            CellOutcome::CouldNotRun {
                status: "NOT-STARTED".to_string(),
                detail: vec![why],
            }
        } else if cell.triple == host {
            CellOutcome::Skipped {
                why: "this machine's own triple, judged by its own build".to_string(),
                fix: "targo --unverified verify lint test".to_string(),
            }
        } else {
            check_cell(ctx, &stock, cell, &policy, &mut members)
        };
        let took = format!("{label}: {}s", started.elapsed().as_secs());
        record_cell(r, &label, took, outcome);
    }
    if owes_win32_oracles(&cells, &ctx.scope, ctx.cells_pulled) {
        win32_oracles(ctx, r, &stock);
    }
    audit_policy(ctx, r, &policy, &cells);
}

/// The checkout's stock pin as the `rustup` row (looked up on the run's `PATH`) holds it: the one
/// toolchain every cell and both windows-sys oracles compile with.
fn stock_of(ctx: &Ctx) -> Result<rustup::Stock, rustup::Unready> {
    let row = aterm_uds::host::tool("rustup");
    let found = row.locate_in(&aterm_uds::host::Lookup {
        path: Some(ctx.env.path.clone()),
        ..aterm_uds::host::Lookup::host(row)
    });
    match found {
        Ok(rustup) => rustup::pinned(rustup.as_path(), &ctx.root),
        Err(absent) => Err(rustup::Unready {
            why: absent.why,
            fix: absent.fix,
        }),
    }
}

/// The stock pin's drivers for `triple`, linted when `clippy`: a COULD NOT RUN's words where the
/// machine holds no pin, or a pin without that std or clippy ([`rustup::Stock::serving`]).
fn serving<'s>(
    stock: &'s Result<rustup::Stock, rustup::Unready>,
    triple: &str,
    clippy: bool,
) -> Result<&'s rustup::Toolchain, rustup::Unready> {
    stock
        .as_ref()
        .map_err(Clone::clone)
        .and_then(|stock| stock.serving(triple, clippy))
}

/// Why the run's end keeps the next cell from starting, if it does: the halt's words once the
/// run's first proven red ended it ([`Ctx::halted`]), else the interrupt's once one is ending the
/// run. A cell started then would find every child it spawns refused, and its row would name the
/// resolver's failure (`UNRESOLVED`) rather than the run's end.
fn cell_ended(ctx: &Ctx) -> Option<String> {
    ctx.halted().map(str::to_owned).or_else(|| {
        crate::stopping().then(|| {
            "the run was interrupted before this cell started, so it decided nothing".to_owned()
        })
    })
}

/// One cell's row under `label`, then how long it took and what it printed. A COULD NOT RUN row
/// names its cause (the first line of what the machine said: a spawn error, a full disk, the
/// run's interrupt, a toolchain's refusal, cut to 240 characters with its fix kept,
/// [`shortened`]) in the row itself, as the check's terminal line shows only the row: the lines
/// under it reach the log alone.
fn record_cell(r: &mut Report, label: &str, took: String, outcome: CellOutcome) {
    match outcome {
        CellOutcome::Unselected => r.pass(format!(
            "{label} — the selection holds nothing this cell builds for its target, so it \
             cannot change what the cell compiles (--all checks the whole graph)"
        )),
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
            let cause = detail
                .iter()
                .flat_map(|d| d.lines())
                .map(str::trim)
                .find(|l| !l.is_empty());
            r.cannot_run(match cause {
                Some(cause) => format!("{label} — {status}: {}", shortened(cause, 240)),
                None => format!("{label} — {status}: the cell's check named no cause"),
            });
            r.raw(took);
            r.raw(detail.join("\n"));
        }
    }
}

/// `cause` in at most `width` characters, a trailing ` (fix: …)` kept whole after an ellipsis:
/// the row names what provides the missing piece though the cause before it runs long.
fn shortened(cause: &str, width: usize) -> String {
    if cause.chars().count() <= width {
        return cause.to_string();
    }
    let fix = cause
        .rfind(" (fix: ")
        .filter(|_| cause.ends_with(')'))
        .map(|at| &cause[at..])
        .filter(|fix| fix.chars().count() < width);
    match fix {
        Some(fix) => {
            let head: String = cause
                .chars()
                .take(width - 1 - fix.chars().count())
                .collect();
            format!("{head}…{fix}")
        }
        None => cause.chars().take(width).collect(),
    }
}

/// The windows-sys oracles (`aterm-win32-oracle`'s two verbs) and what each holds to its
/// registry crate version (STOCK EXCEPTION: the checkout's stock pin compiles Windows std).
pub const WIN32_ORACLES: [(&str, &str); 2] = [
    (
        "oracle",
        "the windows-sys 0.61 facade and aterm-win32's closure against registry windows-sys 0.61.2, its hand-written COM, DXGI, Direct3D 11, DirectComposition and UI Automation against windows 0.62.2",
    ),
    (
        "oracle-052",
        "the windows-sys 0.52 facade (crates/aterm-windows-sys-052) against registry windows-sys 0.52.0",
    ),
];

/// The packages the [`WIN32_ORACLES`] judge: `crates/aterm-win32`, and the windows-sys facades
/// (`crates/aterm-windows-sys`, which re-exports it, and `crates/aterm-windows-sys-052`), each
/// the package `windows-sys`. A pulled or `-p` run owes the oracles only for a selection holding
/// one: they judge those crates alone (255 s cold here, 2026-10-26).
pub const WIN32_ORACLE_SUBJECTS: [&str; 2] = ["aterm-win32", "windows-sys"];

/// The packages a pulled run owes the oracles for only when the change touched them
/// ([`crate::scope::Scope::touches`]): the driver, and `aterm-census`, whose lexer tokenizes the
/// Windows sources the oracle judges, so a change there alters what the oracle reads. The driver
/// sits in the cone of each of its dependencies, and a change to one of its other four (`aterm-uds`,
/// `aterm-json`, `aterm-toml`, `aterm-tempfile`) that breaks its end-to-end run, past its own unit
/// tests, shows only at the next `verify cells` or the next change to a subject.
pub const WIN32_ORACLE_DRIVERS: [&str; 2] = ["aterm-win32-oracle", "aterm-census"];

/// Does this run check a Windows cell, which brings the [`WIN32_ORACLES`]?
#[must_use]
pub fn runs_win32_oracles(cells: &[Cell]) -> bool {
    cells.iter().any(|c| c.triple.ends_with("-pc-windows-msvc"))
}

/// Does this run owe the [`WIN32_ORACLES`]? It checks a Windows cell and it did not narrow them
/// away: its selection holds a subject (the whole workspace holds them all) or touched a driver
/// ([`WIN32_ORACLE_DRIVERS`]), or it names `cells` with no `-p`.
#[must_use]
pub fn owes_win32_oracles(cells: &[Cell], scope: &crate::scope::Scope, pulled: bool) -> bool {
    use crate::scope::Scope;
    runs_win32_oracles(cells)
        && ((!pulled && matches!(scope, Scope::Changed(_)))
            || WIN32_ORACLE_SUBJECTS
                .iter()
                .any(|name| scope.includes_crate(name))
            || WIN32_ORACLE_DRIVERS.iter().any(|name| scope.touches(name)))
}

/// `targo --unverified run -q -p aterm-win32-oracle --target-dir <cells lane>/win32-oracle-bin
/// -- <verb> --root <R> --stock <pin>`, from `<R>`: the oracle compiles Windows std on the
/// checkout's stock pin, as every cell does.
///
/// # Errors
/// The current-directory error from making a relative path absolute.
pub fn win32_oracle_cmd(ctx: &Ctx, verb: &str, stock: &str) -> std::io::Result<Cmd> {
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
            "--stock".to_string(),
            stock.to_string(),
        ]),
    );
    cmd.cwd = Some(root);
    Ok(cmd)
}

/// The reason an oracle that decided nothing gives on its last `… could not run: <reason>`
/// line.
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
/// oracle's: 0 passes, 1 is a finding, 3 or a signal decided nothing.
fn win32_oracles(ctx: &Ctx, r: &mut Report, stock: &Result<rustup::Stock, rustup::Unready>) {
    let windows = serving(stock, "x86_64-pc-windows-msvc", false)
        .and_then(|_| serving(stock, "aarch64-pc-windows-msvc", false));
    for (verb, holds) in WIN32_ORACLES {
        let label = format!("win32 {verb} ({holds}; x86_64 and aarch64 pc-windows-msvc)");
        if !ctx.tools.have_targo() {
            r.skip(format!("{label} — no targo to build the driver"));
            continue;
        }
        let toolchain = match &windows {
            Ok(toolchain) => toolchain,
            Err(unready) => {
                r.cannot_run(format!("{label} — {unready}"));
                continue;
            }
        };
        let cmd = match win32_oracle_cmd(ctx, verb, &toolchain.name) {
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
        r.record_oracle(&out, outcome, &label, why);
    }
}

/// The `cshim` rows no cell of this run used: one naming a triple no forge
/// cell has is dead; one whose cell did not run is held to that cell's graph
/// unless `--cell` narrowed the run.
fn audit_policy(ctx: &Ctx, r: &mut Report, policy: &[Shim], ran: &[Cell]) {
    let all = aterm_forge::resolve::default_cells();
    let mut graphs: BTreeMap<String, Result<(Graph, BTreeSet<PkgId>), String>> = BTreeMap::new();
    let mut members: Option<Result<Members, String>> = None;
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
        if !ctx.cells.is_empty() || ctx.cells_pulled {
            unread = true;
            continue;
        }
        let graph = graphs.entry(cell.name.clone()).or_insert_with(|| {
            let extra = members
                .get_or_insert_with(|| member_names(ctx))
                .clone()?
                .roots_of(cell);
            graph_of(ctx, cell, &extra).map(|(g, paths)| {
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
            "{POLICY}: the rows of cells this run did not check were not read (--cell, or a \
             pulled run)"
        ));
    }
}

/// Whether `shim` is live in the lint's graph and in the test-target pass's: its package at
/// its version is in each.
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
/// version bump does: its build script was never read.
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

/// A member selection's target packages and complete build inputs.
#[derive(Debug, Default)]
pub struct MemberGraph {
    pub nodes: BTreeSet<PkgId>,
    pub selected: Vec<String>,
    pub excluded: Vec<String>,
    /// Each root's complete build inputs, for [`dependents_in_graph`].
    pub dependencies: BTreeMap<String, BTreeSet<PkgId>>,
}

/// The test-target pass's graph for `cell`: the selection
/// [`test_selection`] makes from `members` and the cell's `normal` package
/// names, resolved by `targo check --unit-graph` for exactly that selection.
fn test_graph(
    ctx: &Ctx,
    cell: &Cell,
    members: &Members,
    normal: &BTreeSet<String>,
) -> Result<MemberGraph, String> {
    member_graph(ctx, cell, members, normal, true)
}

fn member_graph(
    ctx: &Ctx,
    cell: &Cell,
    members: &Members,
    normal: &BTreeSet<String>,
    all_targets: bool,
) -> Result<MemberGraph, String> {
    let patched = patched_crates(&ctx.root)?;
    let (selected, excluded) = if all_targets {
        test_selection(&members.names, normal, &patched)
    } else {
        let selected: BTreeSet<String> = std::iter::once(cell.package.clone())
            .chain(members.roots_of(cell))
            .collect();
        let excluded = members.names.difference(&selected).cloned().collect();
        (selected.into_iter().collect(), excluded)
    };
    if selected.is_empty() {
        return Err(format!(
            "no workspace member is in cell `{}`'s graph, so its member pass would \
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
        move || member_units(&targo, &path, &root, &triple, &excluded, all_targets)
    })
    .unwrap_or_else(|why| Err(format!("`targo check --unit-graph` {}", why.sentence())))?;
    let excluded_set: BTreeSet<String> = excluded.iter().cloned().collect();
    let roots: BTreeSet<String> = members.names.difference(&excluded_set).cloned().collect();
    let dependencies = units::read(&out, &cell.triple, &roots)?;
    Ok(MemberGraph {
        nodes: dependencies
            .targets
            .values()
            .flat_map(|nodes| nodes.iter().cloned())
            .collect(),
        selected,
        excluded,
        dependencies: dependencies.inputs,
    })
}

/// Resolve selected target kinds without compiling or running build scripts. The compact
/// unit graph retains host/target and feature distinctions.
/// The pinned Trust Cargo resolves this graph, as it does for the existing test shaper;
/// the later stock rustup compiler is never substituted for this resolver.
fn member_units(
    targo: &Path,
    path: &std::ffi::OsStr,
    root: &Path,
    triple: &str,
    excluded: &[String],
    all_targets: bool,
) -> Result<String, String> {
    let mut cmd = aterm_uds::spawnfd::command(targo);
    cmd.env("PATH", path)
        .current_dir(root)
        .args(["--unverified", "check"])
        .arg("--manifest-path")
        .arg(root.join("Cargo.toml"))
        .args(["--workspace", "--locked", "--offline"])
        .args(crate::stages::UNIT_GRAPH)
        .args(["--target", triple]);
    if all_targets {
        cmd.arg("--all-targets");
    }
    for name in excluded {
        cmd.arg("--exclude").arg(name);
    }
    let out = cmd
        .output_owned()
        .map_err(|e| format!("could not run {} check --unit-graph: {e}", targo.display()))?;
    if !out.status.success() {
        return Err(format!(
            "{triple} requires pinned Trust Cargo's check --unit-graph -Z unstable-options capability; resolution failed without compiling: {}",
            String::from_utf8_lossy(&out.stderr).trim_end()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The cell's packages compiled for the host ([`host_side`]). A third-party
/// proc macro has no path in the tree's output: forge finds its directory in
/// the registry.
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
/// the run's ceiling: its root package's, with each of `extra`'s joined on
/// ([`resolve_roots`]).
fn graph_of(
    ctx: &Ctx,
    cell: &Cell,
    extra: &[String],
) -> Result<(Graph, BTreeMap<PkgId, PathBuf>), String> {
    let (cargo, root, owned, extra) = (
        ctx.forge_cargo(),
        ctx.root.clone(),
        cell.clone(),
        extra.to_vec(),
    );
    crate::exec::bounded(ctx.child_ceiling, move || {
        let mut log = String::new();
        resolve_roots(&cargo, &root, &owned, &extra, &mut log)
    })
    .unwrap_or_else(|why| Err(format!("`targo tree` {}", why.sentence())))
}

/// `cell`'s graph with every `extra` root's joined on: forge resolves each
/// root's normal-edge graph for the cell's triple, and each extra root hangs
/// off the cell's root as though the root depended on it, so the host side
/// ([`host_side`]) and the lint's library are judged from the one root.
///
/// # Errors
/// A root forge could not resolve.
pub fn resolve_roots(
    cargo: &aterm_forge::resolve::Cargo,
    root: &Path,
    cell: &Cell,
    extra: &[String],
    log: &mut String,
) -> Result<(Graph, BTreeMap<PkgId, PathBuf>), String> {
    let (mut graph, mut paths) = aterm_forge::resolve::graph_and_paths(cargo, root, cell, log)?;
    for package in extra {
        let one = Cell {
            package: package.clone(),
            ..cell.clone()
        };
        let (more, more_paths) = aterm_forge::resolve::graph_and_paths(cargo, root, &one, log)?;
        graph
            .edges
            .entry(graph.root.clone())
            .or_default()
            .insert(more.root.clone());
        graph.nodes.extend(more.nodes);
        for (from, to) in more.edges {
            graph.edges.entry(from).or_default().extend(to);
        }
        paths.extend(more_paths);
    }
    Ok((graph, paths))
}

/// One cell, end to end: its graph, its shims, its toolchain (the stock pin, `stock`), the lint of
/// its roots, and (off wasm32) the all-targets pass over its workspace members; of [`narrows`] a
/// selection, the lint and that pass of the selected members, in a cone run with the dependents in
/// this cell's graph of what the change touched ([`wanted_in_cell`]). A pin that cannot serve the
/// cell ([`serving`]) is a COULD NOT RUN (`NO-TOOLCHAIN`) naming what provides the missing piece
/// (provision's rustup row adds every cell's std and clippy to the pin,
/// [`crate::provision::CELL_TARGETS`]).
fn check_cell(
    ctx: &Ctx,
    stock: &Result<rustup::Stock, rustup::Unready>,
    cell: &Cell,
    policy: &[Shim],
    members: &mut Option<Result<Members, String>>,
) -> CellOutcome {
    let failed = |status: &str, detail: Vec<String>| CellOutcome::Failed {
        status: status.to_string(),
        detail,
    };
    let could_not = |status: &str, detail: Vec<String>| CellOutcome::CouldNotRun {
        status: status.to_string(),
        detail,
    };
    let members = match members.get_or_insert_with(|| member_names(ctx)) {
        Ok(m) => &*m,
        Err(e) => return could_not("NO-MEMBERS", vec![e.clone()]),
    };
    let extra = members.roots_of(cell);
    let (graph, paths) = match graph_of(ctx, cell, &extra) {
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
    let selects = ctx.cells_pulled || matches!(ctx.scope, crate::scope::Scope::Packages(_));
    let changed = matches!(ctx.scope, crate::scope::Scope::Changed(_));
    if selects && !changed && selected_in(&own, &ctx.scope).is_empty() {
        return CellOutcome::Unselected;
    }
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
        match test_graph(ctx, cell, members, &graph_names) {
            Ok(t) => Some(t),
            Err(e) => return could_not("NO-TEST-GRAPH", vec![e]),
        }
    };
    let test_nodes = tests.as_ref().map(|t| t.nodes.clone()).unwrap_or_default();
    // Browser cells do not check test targets, but their ordinary build still consumes host
    // tools. Resolve only their default target kinds when closing a change cone.
    let impact = if tests.is_none() && changed {
        match member_graph(ctx, cell, members, &graph_names, false) {
            Ok(graph) => Some(graph),
            Err(error) => return could_not("NO-IMPACT-GRAPH", vec![error]),
        }
    } else {
        None
    };
    let inputs = tests
        .as_ref()
        .or(impact.as_ref())
        .map(|graph| &graph.dependencies);
    let wanted = wanted_in_cell(&graph_names, inputs, &ctx.scope);
    if selects && wanted.is_disjoint(&own) {
        return CellOutcome::Unselected;
    }
    // A patched crate's name can stand for two packages (the windows-sys facades), so neither `-p`
    // nor a read by name could say which was compiled: that selection takes the whole graph.
    let patched = match patched_crates(&ctx.root) {
        Ok(p) => p,
        Err(e) => return could_not("NO-TEST-GRAPH", vec![e]),
    };
    let narrow =
        narrows(&cell.triple, &ctx.scope, ctx.cells_pulled) && wanted.is_disjoint(&patched);
    let judged: BTreeSet<String> = if narrow {
        narrowed_lint(&own, &not_owed, &members.names, &wanted)
    } else {
        own.difference(&not_owed).cloned().collect()
    };
    if narrow && judged.is_empty() {
        return CellOutcome::Unselected;
    }

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

    let toolchain = match serving(stock, &cell.triple, true) {
        Ok(toolchain) => toolchain,
        Err(unready) => return could_not("NO-TOOLCHAIN", vec![unready.to_string()]),
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
        toolchain,
        cwd: &cwd,
        target_dir: &target_dir,
        shim_args: &shim_args,
    };
    let discovery = match GitDiscovery::load(ctx, &cwd) {
        Ok(discovery) => discovery,
        Err(error) => return could_not("NO-SOURCE-CONTEXT", vec![error]),
    };

    let packages = if narrow {
        package_args(&judged, &ctx.scope)
    } else {
        lint_packages(cell, &extra)
    };
    let lint = exec::run(&cargo.lint(&packages), ctx.exec_env());
    let mut red = Red {
        leaves_test_warnings: leaves_test_warnings(ctx.cells_pulled, &cell.triple),
        ..Red::default()
    };
    let read = match read_cargo_run_with_discovery(
        &lint,
        &cell.triple,
        &graph_names,
        false,
        &mut red,
        Some(&discovery),
    ) {
        Ok(read) => read,
        Err(outcome) => return outcome,
    };
    // A narrowed build need not reach a shimmed package at all; its whole-graph run judges the row.
    let inert = shimmed
        .iter()
        .filter(|p| !narrow && !read.checked.contains(*p));
    for pkg in inert {
        red.note(
            format!("SHIM-INERT({pkg})"),
            format!(
                "{POLICY} shims `{pkg}` and it still did not type-check: check the row's `links` \
                 key against the package's manifest (cargo ignores an override keyed to a name \
                 no package links)"
            ),
        );
    }
    let owed: Vec<&String> = judged.iter().collect();
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
        "{}/{} {} crates linted on `{}`{}",
        owed.len() - unread.len(),
        owed.len(),
        if narrow { "selected" } else { "own" },
        toolchain.name,
        not_owed_note(&proc_macros, &host_deps)
    );
    // A narrowed run used neither the cell's roots nor, necessarily, its shims: only what it ran.
    if !narrow && !extra.is_empty() {
        summary.push_str(&format!(
            ", roots {} and {} ({CELLS_KEY})",
            cell.package,
            extra.join(", ")
        ));
    }
    if !narrow && !shimmed.is_empty() {
        summary.push_str(&format!(", shimmed (not linked): {}", shimmed.join(", ")));
    }
    if !narrow && !test_shimmed.is_empty() {
        summary.push_str(&format!(
            ", test targets shimmed (not linked): {}",
            test_shimmed.join(", ")
        ));
    }

    let lint_only = lint_only_in_run(ctx.cells_pulled, &cell.triple, || host_triple(ctx));
    if let (None, Some(tests), false) = (&red.status, &tests, lint_only) {
        let test_cargo = CellCargo {
            shim_args: &test_shim_args,
            ..cargo
        };
        let targets: Vec<&String> = tests
            .selected
            .iter()
            .filter(|name| !narrow || wanted.contains(*name))
            .collect();
        let check = if narrow {
            test_cargo.selected_targets(&package_args(targets.iter().copied(), &ctx.scope))
        } else {
            test_cargo.all_targets(&tests.excluded)
        };
        let run = exec::run(&check, ctx.exec_env());
        let read = match read_cargo_run_with_discovery(
            &run,
            &cell.triple,
            &graph_names,
            true,
            &mut red,
            Some(&discovery),
        ) {
            Ok(read) => read,
            Err(outcome) => return outcome,
        };
        let dependency_names: BTreeSet<String> =
            tests.nodes.iter().map(|id| id.name.clone()).collect();
        let dependencies = read_cargo_json(&run.output, &cell.triple, &dependency_names, false);
        for pkg in test_shimmed
            .iter()
            .filter(|p| !narrow && !dependencies.checked.contains(*p))
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
        let owed: Vec<&String> = targets
            .iter()
            .copied()
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
    if !red.discovery.is_empty() {
        summary.push_str(&format!(
            "; non-selected Git manifest diagnostics: {}",
            red.discovery.join("; ")
        ));
    }
    if !red.unjudged.is_empty() {
        summary.push_str(&format!(
            "; {} test-target warning(s) this pulled run does not judge (`targo --unverified verify \
             cells --cell {}` does): {}",
            red.unjudged.len(),
            cell.name,
            red.unjudged.join("; ")
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
    discovery: Vec<String>,
    /// The all-targets pass's rustc warnings are named in `unjudged` rather than found
    /// ([`leaves_test_warnings`]).
    leaves_test_warnings: bool,
    unjudged: Vec<String>,
}

impl Red {
    fn note(&mut self, status: String, line: String) {
        self.status.get_or_insert(status);
        self.detail.push(line);
    }
}

/// One compiler pass with no source-discovery context (unit fixtures).
#[cfg(test)]
fn read_cargo_run(
    run: &exec::Run,
    triple: &str,
    graph: &BTreeSet<String>,
    tests: bool,
    red: &mut Red,
) -> Result<CargoRead, CellOutcome> {
    read_cargo_run_with_discovery(run, triple, graph, tests, red, None)
}

/// Canonical selected manifests and their actual Git checkout boundaries.
/// Cargo may report errors from other manifests it discovers in a Git tree
/// while successfully compiling every selected package.
#[derive(Default)]
struct GitDiscovery {
    cwd: PathBuf,
    manifests: BTreeSet<PathBuf>,
    roots: BTreeSet<PathBuf>,
}

impl GitDiscovery {
    fn load(ctx: &Ctx, cwd: &Path) -> Result<Self, String> {
        let cargo = aterm_forge::resolve::Cargo::pinned(
            ctx.tools.targo.as_os_str(),
            ctx.path_env.as_os_str(),
        );
        let packages = aterm_forge::resolve::git_packages(&cargo, &ctx.root)?;
        let mut out = Self {
            cwd: cwd.to_path_buf(),
            ..Self::default()
        };
        for package in packages.values() {
            let manifest = package
                .directory
                .join("Cargo.toml")
                .canonicalize()
                .map_err(|error| format!("cannot identify Git package manifest: {error}"))?;
            out.manifests.insert(manifest);
            if let Some(root) = package
                .directory
                .ancestors()
                .find(|path| path.join(".git").exists() && path.join(".cargo-ok").is_file())
            {
                // Ancestor manifests may supply inherited workspace inputs.
                // They are never classified as unrelated discovery fixtures.
                for ancestor in package.directory.ancestors() {
                    let inherited = ancestor.join("Cargo.toml");
                    if inherited.is_file() {
                        out.manifests.insert(
                            inherited
                                .canonicalize()
                                .map_err(|error| error.to_string())?,
                        );
                    }
                    if ancestor == root {
                        break;
                    }
                }
                out.roots
                    .insert(root.canonicalize().map_err(|error| error.to_string())?);
            }
        }
        Ok(out)
    }

    fn unselected_manifest(&self, location: &str) -> Option<PathBuf> {
        let mut parts = location.trim().strip_prefix("--> ")?.rsplitn(3, ':');
        if parts.next()?.parse::<u32>().ok()? == 0 || parts.next()?.parse::<u32>().ok()? == 0 {
            return None;
        }
        let path = self.cwd.join(parts.next()?).canonicalize().ok()?;
        if path.file_name()? != "Cargo.toml" || self.manifests.contains(&path) {
            return None;
        }
        self.roots
            .iter()
            .any(|root| path.starts_with(root))
            .then_some(path)
    }
}

/// Positive Cargo completion is required before any diagnostic can be a
/// nonfatal discovery note. An exit code alone is insufficient evidence.
fn cargo_finished_successfully(output: &str) -> bool {
    let mut finished = false;
    for line in output.lines().filter(|line| line.starts_with('{')) {
        let Ok(value) = aterm_json::from_str::<aterm_json::Value>(line) else {
            continue;
        };
        if value.get("reason").and_then(aterm_json::Value::as_str) == Some("build-finished") {
            if value.get("success").and_then(aterm_json::Value::as_bool) != Some(true) {
                return false;
            }
            finished = true;
        }
    }
    finished
}

/// One compiler pass's diagnostics, shared by the lint and all-targets pass.
fn read_cargo_run_with_discovery(
    run: &exec::Run,
    triple: &str,
    graph: &BTreeSet<String>,
    tests: bool,
    red: &mut Red,
    discovery: Option<&GitDiscovery>,
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
    let warnings = if tests && red.leaves_test_warnings {
        red.unjudged
            .extend(warnings.iter().take(20).map(|(pkg, rendered)| {
                let first = rendered.lines().next().unwrap_or_default();
                format!("in `{pkg}`: {first}")
            }));
        &[]
    } else {
        warnings
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
    let lines: Vec<_> = run.output.lines().collect();
    let completed = run.ok && cargo_finished_successfully(&run.output);
    for (index, line) in lines.iter().enumerate().filter(|(_, line)| {
        line.starts_with("error") && !line.starts_with("error: could not compile")
    }) {
        if completed
            && let Some(path) =
                discovery.and_then(|context| context.unselected_manifest(lines.get(index + 1)?))
        {
            red.discovery.push(format!("{}: {line}", path.display()));
            continue;
        }
        red.note(
            if tests {
                "TESTS-CARGO-ERROR"
            } else {
                "CARGO-ERROR"
            }
            .to_string(),
            (*line).to_owned(),
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

/// The two cargo runs one cell makes, sharing its toolchain, cwd and target dir; each pass
/// carries its own shims.
struct CellCargo<'a> {
    ctx: &'a Ctx,
    triple: &'a str,
    toolchain: &'a rustup::Toolchain,
    cwd: &'a Path,
    target_dir: &'a Path,
    shim_args: &'a [String],
}

impl CellCargo<'_> {
    /// Build `<toolchain cargo> <verb> --target <triple>` with this cell's toolchain
    /// environment and job cap.
    fn base(&self, verb: &str) -> Cmd {
        let mut path = OsString::new();
        if let Some(bin) = self.toolchain.cargo.parent() {
            path.push(bin);
            if !self.ctx.path_env.is_empty() {
                path.push(if cfg!(windows) { ";" } else { ":" });
            }
        }
        path.push(&self.ctx.path_env);
        let mut cmd = Cmd::new(&self.toolchain.cargo)
            .env("RUSTC", &self.toolchain.rustc)
            .env("RUSTUP_TOOLCHAIN", &self.toolchain.name)
            .env("PATH", path)
            .env("CARGO_TARGET_DIR", self.target_dir)
            // A lint keeps nothing it could reuse worth its incremental state: 6.1 of the
            // 6.6 GiB linux-arm's directory held after a cold run (2026-10-26).
            .env("CARGO_INCREMENTAL", "0")
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

    /// The lint of `packages` (`-p` arguments): of clippy, only the `clippy.toml` bans (every other
    /// clippy lint is the host's), the browser cells with their own clock-only config; rustc's own
    /// warnings are read from the stream ([`is_cell_warning`]). Under `--release`, so debug
    /// assertions are off as a shipped build has them: a seam gated on them that shipped code calls
    /// is a compile error here, and code only they keep alive a dead-code warning. No dev check
    /// compiles aterm's library that way otherwise; the all-targets pass stays debug-on, because
    /// test targets call those seams. `tippy -p aterm --release` stays in the release set for what
    /// only Trust compiles (`cfg(trust_verify)`, `cfg(clean_islands)`).
    fn lint(&self, packages: &[String]) -> Cmd {
        let mut cmd = self
            .base("clippy")
            .arg("--release")
            .args(packages.iter().cloned());
        cmd = cmd.arg("--").args(lint_flags(self.triple));
        if self.triple.starts_with("wasm32") {
            cmd = cmd.env("CLIPPY_CONF_DIR", self.ctx.root.join(WASM_CLIPPY_CONF));
        }
        cmd
    }

    /// Every target of `packages` (`-p` arguments), type-checked.
    fn selected_targets(&self, packages: &[String]) -> Cmd {
        self.base("check")
            .arg("--all-targets")
            .args(packages.iter().cloned())
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

/// What a foreign cell's lint denies after `--`: the `clippy.toml` bans (and, off wasm, an
/// `#[expect]` no ban fulfils).
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

/// The `-p` arguments of a cell's lint: its root package, then each extra root.
#[must_use]
pub fn lint_packages(cell: &Cell, extra: &[String]) -> Vec<String> {
    std::iter::once(&cell.package)
        .chain(extra)
        .flat_map(|p| ["-p".to_string(), p.clone()])
        .collect()
}

/// Where a member names the cells it ships for outside their root packages'
/// graphs (the Linux lane's guest agent, which nothing in `aterm` depends on):
/// `[package.metadata.aterm] cells = ["linux-arm"]`. Each cell it names lints
/// it and type-checks its targets beside the cell's root.
pub const CELLS_KEY: &str = "[package.metadata.aterm] cells";

/// The workspace's members, from `targo metadata --no-deps`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Members {
    /// Every member's package name, which `--exclude` takes (not always its directory's).
    pub names: BTreeSet<String>,
    /// Each cell's extra roots: the members whose manifests name it under
    /// [`CELLS_KEY`].
    pub roots: BTreeMap<String, BTreeSet<String>>,
}

impl Members {
    /// The members `cell` checks beside its root package, in name order.
    #[must_use]
    pub fn roots_of(&self, cell: &Cell) -> Vec<String> {
        self.roots
            .get(&cell.name)
            .into_iter()
            .flatten()
            .filter(|name| **name != cell.package)
            .cloned()
            .collect()
    }
}

/// The workspace's members, from `targo metadata --no-deps`.
fn member_names(ctx: &Ctx) -> Result<Members, String> {
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
    parse_members(
        &String::from_utf8_lossy(&out.stdout),
        &aterm_forge::resolve::default_cells(),
    )
}

/// The members in `cargo metadata --no-deps` JSON: their names, and the cells
/// each names under [`CELLS_KEY`], every one of which must be one of `cells`.
///
/// # Errors
/// Not JSON, no `packages`, or a member's [`CELLS_KEY`] that is not a list of
/// `cells`' names.
pub fn parse_members(json: &str, cells: &[Cell]) -> Result<Members, String> {
    let value = aterm_json::from_str::<aterm_json::Value>(json)
        .map_err(|e| format!("`targo metadata --no-deps` did not print JSON: {e}"))?;
    let mut members = Members::default();
    let packages = value
        .get("packages")
        .and_then(aterm_json::Value::as_array)
        .into_iter()
        .flatten();
    for package in packages {
        let Some(name) = package.get("name").and_then(aterm_json::Value::as_str) else {
            continue;
        };
        members.names.insert(name.to_string());
        let Some(named) = package.pointer("/metadata/aterm/cells") else {
            continue;
        };
        let known = || {
            cells
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let list = named.as_array().ok_or_else(|| {
            format!(
                "`{name}`'s {CELLS_KEY} is not a list of cell names ({})",
                known()
            )
        })?;
        for cell in list {
            let cell = cell
                .as_str()
                .filter(|c| cells.iter().any(|known| known.name == *c))
                .ok_or_else(|| {
                    format!(
                        "`{name}`'s {CELLS_KEY} names {cell:?}, which is no cell ({})",
                        known()
                    )
                })?;
            members
                .roots
                .entry(cell.to_string())
                .or_default()
                .insert(name.to_string());
        }
    }
    if members.names.is_empty() {
        return Err("`targo metadata --no-deps` listed no workspace members".to_string());
    }
    Ok(members)
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
    let text = crate::read_regular_text(&manifest)
        .map_err(|e| format!("could not read {} ({e})", manifest.display()))?;
    let mut out = BTreeSet::new();
    let mut inside = false;
    let mut subtable: Option<(String, Option<String>)> = None;
    for raw in text.lines() {
        let line = strip_toml_comment(raw).trim();
        if line.starts_with('[') {
            if let Some((key, renamed)) = subtable.take() {
                let name = renamed.unwrap_or(key);
                if !name.is_empty() {
                    out.insert(name);
                }
            }
            inside = matches!(
                line,
                "[patch.crates-io]" | "[patch.\"crates-io\"]" | "[patch.'crates-io']"
            );
            subtable = patch_crates_io_subtable(line).map(|k| (k, None));
            continue;
        }
        if line.is_empty() {
            continue;
        }
        if let Some((_, ref mut renamed)) = subtable {
            if let Some((key, rhs)) = line.split_once('=')
                && key.trim() == "package"
                && let Some(val) = unquote_toml_string(rhs)
            {
                *renamed = Some(val.to_string());
            }
            continue;
        }
        if !inside {
            continue;
        }
        if let Some((key, spec)) = line.split_once('=') {
            let name = renamed_package(spec)
                .unwrap_or_else(|| key.trim().trim_matches(['"', '\'']).trim());
            if !name.is_empty() {
                out.insert(name.to_string());
            }
        }
    }
    if let Some((key, renamed)) = subtable {
        let name = renamed.unwrap_or(key);
        if !name.is_empty() {
            out.insert(name);
        }
    }
    Ok(out)
}

fn patch_crates_io_subtable(header: &str) -> Option<String> {
    let inner = header.strip_prefix('[')?.strip_suffix(']')?.trim();
    let rest = inner
        .strip_prefix("patch.crates-io.")
        .or_else(|| inner.strip_prefix("patch.\"crates-io\"."))
        .or_else(|| inner.strip_prefix("patch.'crates-io'."))?;
    let key = rest.trim().trim_matches(['"', '\'']).trim();
    (!key.is_empty()).then(|| key.to_string())
}

fn strip_toml_comment(line: &str) -> &str {
    let mut in_quote: Option<char> = None;
    let mut escaped = false;
    for (i, c) in line.char_indices() {
        match in_quote {
            Some('"') if escaped => escaped = false,
            Some('"') if c == '\\' => escaped = true,
            Some(q) if c == q => in_quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => in_quote = Some(c),
            None if c == '#' => return &line[..i],
            None => {}
        }
    }
    line
}

fn unquote_toml_string(rhs: &str) -> Option<&str> {
    let rhs = rhs.trim();
    let quote = rhs.chars().next().filter(|&c| c == '"' || c == '\'')?;
    rhs[1..].split(quote).next().filter(|s| !s.is_empty())
}

/// The `package = "…"` of an inline patch spec, if it has one.
#[must_use]
pub fn renamed_package(spec: &str) -> Option<&str> {
    let spec = strip_toml_comment(spec);
    spec.match_indices("package").find_map(|(at, word)| {
        let before = spec[..at].trim_end();
        if !before.is_empty() && !before.ends_with(['{', ',']) {
            return None;
        }
        let value = spec[at + word.len()..]
            .trim_start()
            .strip_prefix('=')?
            .trim_start();
        unquote_toml_string(value)
    })
}

/// What cargo's JSON stream said.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct CargoRead {
    pub checked: BTreeSet<String>,
    pub errors: Vec<(String, String)>,
    pub warnings: Vec<(String, String)>,
}

/// Whether a run's all-targets pass of `triple` names its rustc warnings without finding them: a
/// Windows cell's in a run the selection pulled. They are mostly `cfg(windows)` test helpers gone
/// dead, and a warning stops no guest `cargo test` (`lab windows exec`), so they would restart this
/// run's slow checks for code it does not run. The pass stays: a TEST-TARGET-ERROR does stop that
/// lane, whose whole-crate run costs 8 GiB and one holder at a time. A run naming `cells` judges
/// these warnings, and the lint pass judges them on shipped code, so a Windows test helper can sit
/// dead until someone runs `verify cells`.
#[must_use]
pub fn leaves_test_warnings(pulled: bool, triple: &str) -> bool {
    pulled && triple.ends_with("-pc-windows-msvc")
}

/// Whether a cell's rustc warnings ([`is_cell_warning`]) are findings.
#[must_use]
pub fn judges_warnings(triple: &str) -> bool {
    let Some(own) = os_of(triple) else {
        return false;
    };
    WARNINGS_JUDGED_OFF_FLEET.contains(&own)
        || FLEET_HOST_TRIPLES
            .iter()
            .any(|host| os_of(host) == Some(own))
}

/// The OSes (a triple past its architecture) no fleet machine runs whose cells still judge
/// warnings ([`judges_warnings`]).
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

/// Column-zero `error` lines printed by cargo itself (excluding `error: could not compile`).
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

/// The summary's ` (… not owed: …)` note: the proc macros and the crates only they reach.
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

/// The cell's packages cargo compiles for the host: every proc macro, and every package only
/// proc macros reach.
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

/// Is `dir` inside `root`, both canonicalised (`/tmp` and `/private/tmp` name one place on
/// macOS)? Where either does not resolve, the two are compared as given.
fn path_is_inside(dir: &Path, root: &Path) -> bool {
    match (dir.canonicalize(), root.canonicalize()) {
        (Ok(d), Ok(r)) => d.starts_with(r),
        _ => dir.starts_with(root),
    }
}

/// Does the manifest in `dir` declare `proc-macro = true` under `[lib]`?
fn is_proc_macro(dir: &Path) -> bool {
    let Ok(text) = crate::read_regular_text(dir.join("Cargo.toml")) else {
        return false;
    };
    let mut in_lib = false;
    for raw in text.lines() {
        let line = strip_toml_comment(raw).trim();
        if line.starts_with('[') {
            in_lib = line == "[lib]";
        } else if in_lib {
            let compact = line.replace([' ', '\t'], "");
            if compact == "proc-macro=true" || compact == "proc_macro=true" {
                return true;
            }
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

    /// A pin that cannot serve a cell names `verify provision` on the row itself, though rustup's
    /// refusal spans lines or runs long.
    #[test]
    fn a_cell_without_its_pin_names_provision_on_its_row_whatever_rustup_said() {
        let label = "cell win (x86_64-pc-windows-msvc)";
        for why in [
            "`rustup which --toolchain 1.97.1 cargo` failed: error: toolchain '1.97.1-aarch64-apple-\
             darwin' is not installed\nhelp: run `rustup toolchain install 1.97.1` to install it"
                .to_string(),
            format!("`rustup target list` failed: {}", "x".repeat(400)),
        ] {
            let unready = rustup::Unready {
                why,
                fix: rustup::PROVISION.into(),
            };
            let mut r = Report::new("cells");
            record_cell(
                &mut r,
                label,
                format!("{label}: 1s"),
                CellOutcome::CouldNotRun {
                    status: "NO-TOOLCHAIN".into(),
                    detail: vec![unready.to_string()],
                },
            );
            let result = crate::report::judge(
                crate::checks::Check::Cells,
                &[&r],
                1.0,
                "",
                "targo --unverified verify cells --cell win".into(),
                None,
            );
            let shown = crate::report::render(&result, 5);
            let first = shown.lines().next().unwrap_or_default();
            assert!(
                first.contains(&format!("(fix: {})", rustup::PROVISION)),
                "{shown}"
            );
        }
        let long = format!("{} (fix: {})", "x".repeat(400), rustup::PROVISION);
        let cut = shortened(&long, 240);
        assert_eq!(cut.chars().count(), 240, "{cut}");
        assert!(
            cut.ends_with(&format!("x… (fix: {})", rustup::PROVISION)),
            "{cut}"
        );
        let bare = "y".repeat(400);
        assert_eq!(
            shortened(&bare, 240),
            "y".repeat(240),
            "no fix: a plain cut"
        );
        assert_eq!(shortened("short (fix: z)", 240), "short (fix: z)");
    }

    /// A cell that could not run says why on the check's terminal line, which shows the row and
    /// nothing written under it: the run's interrupt (the 2026-10-24 bare `COULD-NOT-RUN`), a
    /// full disk, a toolchain's refusal.
    #[test]
    fn a_cell_that_could_not_run_names_its_cause_on_the_terminal_line() {
        let label = "cell linux-arm (aarch64-unknown-linux-gnu)";
        let terminal = |outcome: CellOutcome| {
            let mut r = Report::new("cells");
            record_cell(&mut r, label, format!("{label}: 143s"), outcome);
            let result = crate::report::judge(
                crate::checks::Check::Cells,
                &[&r],
                143.0,
                "",
                "targo --unverified verify cells --cell linux-arm".into(),
                None,
            );
            assert_eq!(result.outcome, Outcome::Fail(Severity::CouldNotRun));
            crate::report::render(&result, 5)
        };
        for (status, detail, cause) in [
            (
                "COULD-NOT-RUN",
                vec!["the run was interrupted while this child ran".to_string()],
                "the run was interrupted while this child ran",
            ),
            (
                "COULD-NOT-RUN",
                vec!["the child ran out of disk (No space left on device)".to_string()],
                "No space left on device",
            ),
            (
                "NO-TOOLCHAIN",
                vec!["\nrustup: toolchain '1.97.1' is not installed\nmore".to_string()],
                "rustup: toolchain '1.97.1' is not installed",
            ),
            (
                "NO-TOOLCHAIN",
                vec![
                    rustup::Unready {
                        why: "`1.97.1`, the checkout's stock pin, has no x86_64-pc-windows-msvc \
                              std"
                        .into(),
                        fix: rustup::PROVISION.into(),
                    }
                    .to_string(),
                ],
                "has no x86_64-pc-windows-msvc std (fix: targo --unverified verify provision \
                 rustup)",
            ),
        ] {
            let shown = terminal(CellOutcome::CouldNotRun {
                status: status.into(),
                detail,
            });
            let first = shown.lines().next().unwrap_or_default();
            assert!(first.contains(&format!("{label} — {status}: ")), "{shown}");
            assert!(first.contains(cause), "{shown}");
        }
        let shown = terminal(CellOutcome::CouldNotRun {
            status: "COULD-NOT-RUN".into(),
            detail: Vec::new(),
        });
        assert!(
            shown
                .lines()
                .next()
                .unwrap_or_default()
                .contains("named no cause"),
            "{shown}"
        );
    }

    /// A cell the run's halt keeps from starting says so on its row, rather than the resolver's
    /// failure every child it spawned would meet (`UNRESOLVED`, the cause three lines under it).
    /// CONTROL: a run its halt has not ended starts it.
    #[test]
    fn a_cell_the_runs_end_kept_from_starting_names_that_end() {
        let ctx = Ctx::new(
            PathBuf::from("/r"),
            crate::scope::Scope::workspace(),
            crate::EnvSnapshot::default(),
            PathBuf::from("/tmp"),
        )
        .with_halt(crate::sched::Halt::at_first_red("the run", |_| {}));
        assert_eq!(cell_ended(&ctx), None, "control: nothing has ended the run");
        assert!(ctx.halt.red("lint"));
        let why = cell_ended(&ctx).expect("the halt ended the run");
        assert_eq!(Some(why.as_str()), ctx.halted());
        let label = "cell linux-arm (aarch64-unknown-linux-gnu)";
        let mut r = Report::new("cells");
        record_cell(
            &mut r,
            label,
            format!("{label}: 0s"),
            CellOutcome::CouldNotRun {
                status: "NOT-STARTED".into(),
                detail: vec![why],
            },
        );
        let row = format!(
            "{label} — NOT-STARTED: {}",
            ctx.halted().unwrap_or_default()
        );
        assert_eq!(
            r.outcomes().collect::<Vec<_>>(),
            [(Outcome::Fail(Severity::CouldNotRun), row.as_str())]
        );
    }

    /// A dev edge holds a cone crate there for its tests only, so a cell's closure starts from what
    /// the change touched. CONTROL: a cone that knows only its crates closes over all of them, which
    /// took a change to a model crate through `aterm-core` to `aterm-gui` in every cell.
    #[test]
    fn a_cell_closes_the_cone_over_what_the_change_touched() {
        let id = |name: &str| PkgId {
            name: name.to_string(),
            version: "1.0.0".to_string(),
        };
        let inputs = BTreeMap::from([
            ("aterm-core".to_string(), BTreeSet::from([id("aterm-core")])),
            (
                "aterm-gui".to_string(),
                BTreeSet::from([id("aterm-gui"), id("aterm-core")]),
            ),
            (
                "aterm-models-misc".to_string(),
                BTreeSet::from([id("aterm-models-misc")]),
            ),
        ]);
        let normal = names(&["aterm-core", "aterm-gui", "aterm-models-misc"]);
        let cone = vec!["aterm-core".to_string(), "aterm-models-misc".to_string()];
        let unseeded = crate::scope::Scope::changed("origin/main", cone, true);
        let seeded = unseeded
            .clone()
            .with_seeds(vec!["aterm-models-misc".to_string()]);
        assert_eq!(
            wanted_in_cell(&normal, Some(&inputs), &seeded),
            names(&["aterm-core", "aterm-models-misc"])
        );
        assert_eq!(
            wanted_in_cell(&normal, Some(&inputs), &unseeded),
            names(&["aterm-core", "aterm-gui", "aterm-models-misc"])
        );
        let touched_core = unseeded.with_seeds(vec!["aterm-core".to_string()]);
        assert!(wanted_in_cell(&normal, Some(&inputs), &touched_core).contains("aterm-gui"));
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

    /// No `--cell` is every cell but the host's, a triple another machine could host
    /// included; a name picks that cell, the host's too; an unknown name is refused.
    #[test]
    fn a_run_checks_every_cell_but_the_hosts_or_each_cell_named() {
        let mac = "aarch64-apple-darwin";
        let all = aterm_forge::resolve::default_cells();
        let default = selected(&[], mac).expect("default");
        assert_eq!(default, foreign_cells(mac));
        let triples: Vec<&str> = default.iter().map(|c| c.triple.as_str()).collect();
        assert!(!triples.contains(&mac), "{triples:?}");
        assert!(
            triples.contains(&"x86_64-apple-darwin")
                && triples.contains(&"x86_64-unknown-linux-gnu"),
            "{triples:?}"
        );
        assert_eq!(default.len() + 1, all.len());
        assert_eq!(
            selected(&[], "riscv64gc-unknown-linux-gnu").expect("no cell's"),
            all
        );
        let named = selected(&["mac-arm".to_string(), "win".to_string()], mac).expect("named");
        let names: Vec<&str> = named.iter().map(|c| c.name.as_str()).collect();
        assert!(
            names.contains(&"mac-arm") && names.contains(&"win"),
            "{names:?}"
        );
        assert_eq!(named.len(), 2);
        assert!(selected(&["plan9".to_string()], mac).is_err());
    }

    /// A pulled run checks one cell per OS other than the host's, the host's architecture's
    /// where the OS has one, and both web modules' cells, whose roots differ.
    #[test]
    fn a_pulled_run_checks_one_cell_per_other_os_of_the_hosts_architecture() {
        let names = |host: &str| -> Vec<String> {
            pulled_cells(host).into_iter().map(|c| c.name).collect()
        };
        assert_eq!(
            names("aarch64-apple-darwin"),
            ["linux-arm", "win-arm", "wasm-cpu", "wasm-gpu", "mac-x64"]
        );
        assert_eq!(
            names("x86_64-unknown-linux-gnu"),
            ["mac-x64", "win", "wasm-cpu", "wasm-gpu", "linux-arm"]
        );
        assert_eq!(
            names("x86_64-apple-darwin"),
            ["linux", "win", "wasm-cpu", "wasm-gpu", "mac-arm"]
        );
        assert_eq!(
            names("riscv64gc-unknown-linux-gnu"),
            ["mac-arm", "win", "wasm-cpu", "wasm-gpu", "linux"],
            "no cell of its architecture: each OS's first"
        );
        assert!(lint_only_when_pulled(
            "x86_64-apple-darwin",
            "aarch64-apple-darwin"
        ));
        assert!(
            !lint_only_when_pulled("aarch64-unknown-linux-gnu", "aarch64-apple-darwin"),
            "CONTROL: another OS's test targets differ in their cfgs"
        );
    }

    /// A pulled run's cell of the host's own OS checks no test target, and a run naming `cells`
    /// checks them all. CONTROL: another OS's cell keeps its test pass in a pulled run, and a host
    /// nothing can name takes nothing away.
    #[test]
    fn only_a_pulled_run_skips_the_test_targets_of_a_cell_of_the_hosts_os() {
        let mac = "aarch64-apple-darwin";
        let host = || Ok(mac.to_owned());
        assert!(lint_only_in_run(true, "x86_64-apple-darwin", host));
        assert!(!lint_only_in_run(false, "x86_64-apple-darwin", host));
        assert!(!lint_only_in_run(true, "aarch64-unknown-linux-gnu", host));
        assert!(!lint_only_in_run(true, "x86_64-apple-darwin", || Err(
            "no host".to_owned()
        )));
    }

    /// `-p` and a pulled run narrow a cell, never the browser's; a run naming `cells` with no `-p`
    /// checks the whole graph, in the change cone or not, as no other check does.
    #[test]
    fn only_p_and_a_pulled_run_narrow_a_cell() {
        use crate::scope::Scope;
        let one = Scope::packages(&["aterm-agent".to_string()]);
        let cone = Scope::changed("origin/main", vec!["aterm-agent".into()], true);
        for triple in ["aarch64-unknown-linux-gnu", "aarch64-pc-windows-msvc"] {
            assert!(narrows(triple, &one, false), "{triple}");
            assert!(narrows(triple, &one, true), "{triple}");
            assert!(narrows(triple, &cone, true), "{triple}");
            assert!(!narrows(triple, &cone, false), "{triple}");
            assert!(!narrows(triple, &Scope::workspace(), false), "{triple}");
            assert!(!narrows(triple, &Scope::workspace(), true), "{triple}");
        }
        assert!(!narrows("wasm32-unknown-unknown", &one, false));
        assert!(!narrows("wasm32-unknown-unknown", &cone, true));
    }

    /// A dependent only this cell's OS has is found in its own graph, dev edges included: the root
    /// `aterm-gui` reaches `aterm-x11` here though this machine's cone could not say so. NEGATIVE
    /// CONTROL: a member that does not reach it is left out.
    #[test]
    fn a_seeds_dependents_come_from_the_cells_own_graph() {
        let ids = |names: &[&str]| {
            names
                .iter()
                .map(|name| PkgId::new(*name, "0.1.0"))
                .collect()
        };
        let graph = BTreeMap::from([
            (
                "aterm-gui".into(),
                ids(&["aterm-gui", "aterm-window", "aterm-x11"]),
            ),
            ("aterm-x11".into(), ids(&["aterm-x11"])),
            ("aterm-grid".into(), ids(&["aterm-grid", "aterm-types"])),
        ]);
        assert_eq!(
            dependents_in_graph(&graph, &names(&["aterm-x11"])),
            names(&["aterm-gui", "aterm-x11"])
        );
        assert_eq!(
            dependents_in_graph(&graph, &names(&["aterm-window"])),
            names(&["aterm-gui"])
        );
        assert!(dependents_in_graph(&graph, &names(&["nothing"])).is_empty());
    }

    /// A `-p` no member answers to is an error, not a green over nothing, spec spellings included.
    #[test]
    fn a_p_naming_no_member_is_reported() {
        use crate::scope::Scope;
        let members = names(&["aterm-agent", "libc"]);
        let packages = |named: &[&str]| {
            Scope::packages(&named.iter().map(|n| (*n).to_string()).collect::<Vec<_>>())
        };
        assert!(unknown_packages(&packages(&["aterm-agent"]), &members).is_empty());
        assert!(unknown_packages(&packages(&["libc@0.2.0"]), &members).is_empty());
        assert_eq!(
            unknown_packages(
                &packages(&["aterm-agnet", "aterm-agent", "aterm-libc"]),
                &members
            ),
            ["aterm-agnet", "aterm-libc"]
        );
        assert!(unknown_packages(&Scope::workspace(), &members).is_empty());
    }

    /// A narrowed lint names selected members built for the target, never host code the cell's
    /// graph builds for this machine.
    #[test]
    fn a_narrowed_lint_names_no_host_code_and_no_non_member() {
        let own = names(&["aterm-agent", "aterm-macros", "astream-sys", "aterm-gen"]);
        let not_owed = names(&["aterm-macros", "aterm-gen"]);
        let members = names(&["aterm-agent", "aterm-macros", "aterm-gen"]);
        assert_eq!(
            narrowed_lint(
                &own,
                &not_owed,
                &members,
                &names(&[
                    "aterm-agent",
                    "aterm-macros",
                    "astream-sys",
                    "aterm-gen",
                    "aterm-objc"
                ])
            ),
            names(&["aterm-agent"])
        );
        assert!(
            narrowed_lint(&own, &not_owed, &members, &names(&["aterm-macros"])).is_empty(),
            "only host code selected: the cell has nothing to compile"
        );
    }

    /// A member whose name the lock shares keeps its path spec, or the `-p` would be ambiguous.
    #[test]
    fn package_args_keep_the_selections_own_spec() {
        use crate::scope::Scope;
        let spec = "path+file:///repo/crates/x#x@1.0.0".to_string();
        let cone = Scope::changed("origin/main", vec![spec.clone()], true);
        assert_eq!(
            package_args(&names(&["x"]), &cone),
            ["-p".to_string(), spec]
        );
        assert_eq!(
            package_args(&names(&["x"]), &Scope::workspace()),
            ["-p", "x"]
        );
    }

    /// The oracles judge the Windows facades alone: a narrowed run owes them only for a selection
    /// holding one, and one naming `cells` with no `-p` always, so an edit to the oracle itself runs them.
    #[test]
    fn the_win32_oracles_are_owed_unless_a_selection_rules_them_out() {
        use crate::scope::Scope;
        let pick = |cell: &str| {
            aterm_forge::resolve::select(
                &aterm_forge::resolve::default_cells(),
                &[cell.to_string()],
            )
            .expect("the cell")
        };
        let (win, linux) = (pick("win"), pick("linux-arm"));
        let packages = |name: &str| Scope::packages(&[name.to_string()]);
        let cone = Scope::changed("origin/main", vec!["aterm-agent".into()], true);
        assert!(!owes_win32_oracles(&win, &packages("aterm-agent"), false));
        assert!(owes_win32_oracles(&win, &packages("aterm-win32"), false));
        assert!(owes_win32_oracles(
            &win,
            &packages("aterm-win32-oracle"),
            false
        ));
        assert!(!owes_win32_oracles(&win, &cone, true));
        assert!(owes_win32_oracles(&win, &cone, false));
        assert!(owes_win32_oracles(&win, &Scope::workspace(), false));
        assert!(
            owes_win32_oracles(&win, &Scope::workspace(), true),
            "a pulled run the selection widened to the whole workspace"
        );
        assert!(!owes_win32_oracles(&linux, &Scope::workspace(), false));
        // The driver is in the cone of each of its dependencies, which does not change what the
        // oracles judge. CONTROL: a change to the driver itself, to aterm-census, which reads the
        // Windows sources for it, or to a subject still owes them.
        let driver_in_cone = |seed: &str| {
            Scope::changed(
                "origin/main",
                vec![seed.to_string(), "aterm-win32-oracle".to_string()],
                true,
            )
            .with_seeds(vec![seed.to_string()])
        };
        assert!(!owes_win32_oracles(
            &win,
            &driver_in_cone("aterm-uds"),
            true
        ));
        assert!(owes_win32_oracles(
            &win,
            &driver_in_cone("aterm-uds"),
            false
        ));
        for seed in ["aterm-census", "aterm-win32-oracle", "aterm-win32"] {
            assert!(
                owes_win32_oracles(&win, &driver_in_cone(seed), true),
                "{seed}"
            );
        }
        let facade = Scope::changed("origin/main", vec!["windows-sys@0.52.0".into()], true);
        assert!(owes_win32_oracles(&win, &facade, true), "by its spec");
    }

    /// A pulled cell is checked for the selected crates its graph holds: the cone's or `-p`'s,
    /// every one under `--all`, and none of a selection that only names crates outside it.
    #[test]
    fn a_pulled_cell_is_checked_only_for_selected_crates_in_its_graph() {
        use crate::scope::Scope;
        let own = names(&["aterm", "aterm-gui", "aterm-grid"]);
        let cone = Scope::changed(
            "origin/main",
            vec!["aterm".into(), "aterm-gui".into(), "aterm-objc".into()],
            true,
        );
        assert_eq!(selected_in(&own, &cone), ["aterm", "aterm-gui"]);
        assert_eq!(
            selected_in(&own, &Scope::packages(&["aterm-grid".to_string()])),
            ["aterm-grid"]
        );
        assert_eq!(selected_in(&own, &Scope::workspace()).len(), 3);
        assert!(
            selected_in(&own, &Scope::packages(&["aterm-objc".to_string()])).is_empty(),
            "a macOS-only crate is in no other OS's graph"
        );
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
    /// version or an unselected member's oracle is live in neither.
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

    /// A real workspace's test-target unit graph (resolved, never compiled) holds only the selected
    /// members and their dev and target dependencies: no build-only or proc-macro subtree, no
    /// excluded member's oracle.
    #[test]
    fn the_test_target_graph_has_only_selected_members_dev_and_target_dependencies() {
        let dir = aterm_tempfile::tempdir().expect("private resolver fixture");
        let root = dir.path();
        let packages = [
            (
                "app",
                "[dependencies]\ndecoder={path='../decoder'}\npm={path='../pm'}\n\
                 [target.'cfg(windows)'.dependencies]\nforeign={path='../foreign',features=['generated']}\n",
            ),
            (
                "foreign",
                "[features]\ngenerated=['dep:build-only']\n[build-dependencies]\nbuild-only={path='../build-only',optional=true}\n",
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
             [[package]]\nname='app'\nversion='1.0.0'\ndependencies=['decoder','foreign','pm']\n\
             [[package]]\nname='build-only'\nversion='1.0.0'\n\
             [[package]]\nname='decoder'\nversion='1.0.0'\ndependencies=['oracle']\n\
             [[package]]\nname='foreign'\nversion='1.0.0'\ndependencies=['build-only']\n\
             [[package]]\nname='host-only'\nversion='1.0.0'\n\
             [[package]]\nname='ignored'\nversion='1.0.0'\ndependencies=['ignored-sys']\n\
             [[package]]\nname='ignored-sys'\nversion='1.0.0'\n\
             [[package]]\nname='oracle'\nversion='1.0.0'\ndependencies=['ignored-sys']\n\
             [[package]]\nname='pm'\nversion='1.0.0'\ndependencies=['host-only']\n",
        )
        .expect("locked local dependency graph");
        let normal: BTreeSet<String> = ["app", "decoder", "foreign", "pm", "host-only"]
            .into_iter()
            .map(str::to_string)
            .collect();
        let (selected, excluded) = test_selection(&members, &normal, &BTreeSet::new());
        assert_eq!(selected, ["app", "decoder", "foreign", "pm"]);
        assert_eq!(excluded, ["build-only", "ignored", "ignored-sys", "oracle"]);
        std::fs::write(
            root.join("foreign/build.rs"),
            "fn main() { panic!(\"graph query executed a build script\"); }",
        )
        .expect("build script that must never run during resolution");
        for kind in ["examples", "benches"] {
            std::fs::create_dir_all(root.join("decoder").join(kind)).unwrap();
            std::fs::write(
                root.join("decoder").join(kind).join("probe.rs"),
                "fn main() {}\n",
            )
            .unwrap();
        }
        let targo = PathBuf::from(env!("CARGO"));
        let path = std::env::var_os("PATH").unwrap_or_default();
        let graph = member_units(
            &targo,
            &path,
            root,
            "x86_64-pc-windows-msvc",
            &excluded,
            true,
        )
        .expect("resolve the actual test member selection without compiling");
        let roots: BTreeSet<String> = members
            .difference(&excluded.iter().cloned().collect())
            .cloned()
            .collect();
        let dependencies = units::read(&graph, "x86_64-pc-windows-msvc", &roots)
            .expect("the selected members' graph");
        let typed =
            crate::unitgraph::Graph::read(&graph, &mut crate::unitgraph::Interner::default())
                .unwrap();
        for kind in ["bench", "example"] {
            assert!(
                typed
                    .roots
                    .iter()
                    .any(|&r| typed.units[r].target_kinds.iter().any(|k| k == kind)),
                "all-targets resolution must include {kind} roots"
            );
        }
        assert_eq!(
            dependents_in_graph(&dependencies.inputs, &BTreeSet::from(["oracle".to_owned()])),
            BTreeSet::from(["decoder".to_owned()]),
            "decoder's own dev oracle is not app's normal dependency"
        );
        let nodes: BTreeSet<PkgId> = dependencies
            .targets
            .values()
            .flat_map(|nodes| nodes.iter().cloned())
            .collect();
        let names: Vec<_> = nodes.iter().map(|id| id.name.as_str()).collect();
        assert_eq!(names, ["app", "decoder", "foreign", "oracle"]);
        let changed = crate::scope::Scope::changed("main", vec!["build-only".into()], true);
        assert_eq!(
            wanted_in_cell(&normal, Some(&dependencies.inputs), &changed),
            BTreeSet::from(["app".into(), "foreign".into()])
        );
        for seed in ["pm", "host-only"] {
            let changed = crate::scope::Scope::changed("main", vec![seed.into()], true);
            assert!(wanted_in_cell(&normal, Some(&dependencies.inputs), &changed).contains("app"));
        }
        assert!(
            wanted_in_cell(
                &normal,
                Some(&dependencies.inputs),
                &crate::scope::Scope::crate_only("build-only")
            )
            .is_empty()
        );
        let browser = member_units(
            &targo,
            &path,
            root,
            "wasm32-unknown-unknown",
            &excluded,
            false,
        )
        .expect("default browser targets resolve without test targets");
        let browser =
            units::read(&browser, "wasm32-unknown-unknown", &roots).expect("browser inputs");
        assert!(wanted_in_cell(&normal, Some(&browser.inputs), &changed).is_empty());
        assert!(
            !browser
                .targets
                .values()
                .any(|nodes| nodes.iter().any(|id| id.name == "oracle"))
        );
        let one: BTreeSet<String> = ["app".to_string()].into_iter().collect();
        assert!(units::read("", "x86_64-pc-windows-msvc", &one).is_err());
        assert!(units::read(&graph, "x86_64-pc-windows-msvc", &one).is_err());
    }

    /// A member a selected proc macro depends on prints as both a target and a host node.
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
        let graph = member_units(&targo, &path, root, "x86_64-pc-windows-msvc", &[], true)
            .expect("resolve the fixture without compiling");
        let dependencies = units::read(&graph, "x86_64-pc-windows-msvc", &members)
            .expect("a member's host copy is not a second target root");
        let nodes: BTreeSet<PkgId> = dependencies
            .targets
            .values()
            .flat_map(|nodes| nodes.iter().cloned())
            .collect();
        let found: Vec<_> = nodes.iter().map(|id| id.name.as_str()).collect();
        assert_eq!(found, ["app", "shared", "win-only"]);
        assert!(
            dependencies.targets["pm"].is_empty(),
            "host roots cannot activate target shims"
        );
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
    /// (a proc macro, a build script) never; test units only when asked.
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
    /// (`dead_code`) or an import only it uses never warns on the host, so the
    /// cell's compile is where it shows.
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
                case_observations: None,
                stopped_at: None,
                hung: None,
                ok: true,
                output: out.clone(),
                code: Some(0),
                spawn_error: None,
                usage: None,
            };
            let mut red = Red::default();
            read_cargo_run(&run, "aarch64-unknown-linux-gnu", &graph, tests, &mut red)
                .expect("the compiler ran");
            assert_eq!(red.status.as_deref(), Some(status));
            assert_eq!(red.detail.len(), 2);
            assert!(red.detail[0].contains("launchctl_list"), "{:?}", red.detail);
        }
        let run = exec::Run {
            case_observations: None,
            stopped_at: None,
            hung: None,
            ok: true,
            output: out.clone(),
            code: Some(0),
            spawn_error: None,
            usage: None,
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
        // A pulled run names a Windows cell's test-target warnings and is not red on them. CONTROL:
        // the same warnings in its lint pass, a run naming `cells`, and a pulled Linux cell.
        let run = exec::Run {
            case_observations: None,
            stopped_at: None,
            hung: None,
            ok: true,
            output: out.clone(),
            code: Some(0),
            spawn_error: None,
            usage: None,
        };
        let read = |triple: &str, pulled: bool, tests: bool| {
            let mut red = Red {
                leaves_test_warnings: leaves_test_warnings(pulled, triple),
                ..Red::default()
            };
            read_cargo_run(&run, triple, &graph, tests, &mut red).expect("the compiler ran");
            (red.status, red.unjudged)
        };
        let win = "aarch64-pc-windows-msvc";
        let (status, unjudged) = read(win, true, true);
        assert_eq!(status, None);
        assert_eq!(unjudged.len(), 2, "{unjudged:?}");
        assert!(unjudged[0].starts_with("in `"), "{unjudged:?}");
        assert_eq!(read(win, true, false).0.as_deref(), Some("WARNING(2)"));
        assert_eq!(
            read(win, false, true).0.as_deref(),
            Some("TEST-TARGET-WARNING(2)")
        );
        assert_eq!(
            read("aarch64-unknown-linux-gnu", true, true).0.as_deref(),
            Some("TEST-TARGET-WARNING(2)")
        );
        let clippy_only = warning(
            r#"{"code":"clippy::duration_suboptimal_units","explanation":null}"#,
            "warning: constructing a `Duration` using a smaller unit",
        );
        let run = exec::Run {
            case_observations: None,
            stopped_at: None,
            hung: None,
            ok: true,
            output: clippy_only,
            code: Some(0),
            spawn_error: None,
            usage: None,
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
            case_observations: None,
            stopped_at: None,
            hung: None,
            ok: code == Some(0),
            output: artifact.to_string(),
            code,
            spawn_error: None,
            usage: None,
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
                    case_observations: None,
                    stopped_at: None,
                    hung: None,
                    ok: false,
                    output: diagnostic.to_string(),
                    code: Some(101),
                    spawn_error: None,
                    usage: None,
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
                    case_observations: None,
                    stopped_at: None,
                    hung: None,
                    ok: false,
                    output,
                    code: None,
                    spawn_error,
                    usage: None,
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
    fn only_successful_unselected_git_manifest_diagnostics_are_nonfatal() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let git = dir.path().join("checkout");
        let selected = git.join("selected/Cargo.toml");
        let inherited = git.join("Cargo.toml");
        let unused = git.join("fixture/Cargo.toml");
        let outside = dir.path().join("outside/Cargo.toml");
        for path in [&selected, &inherited, &unused, &outside] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "fixture").unwrap();
        }
        let context = GitDiscovery {
            cwd: dir.path().to_owned(),
            roots: BTreeSet::from([git.canonicalize().unwrap()]),
            manifests: BTreeSet::from([
                selected.canonicalize().unwrap(),
                inherited.canonicalize().unwrap(),
            ]),
        };
        let graph = names(&["app"]);
        let artifact = r#"{"reason":"compiler-artifact","package_id":"path+file:///r/app#app@1","profile":{"test":false},"filenames":["/t/aarch64-unknown-linux-gnu/app.rmeta"]}"#;
        let diagnostic = |path: &Path| {
            format!(
                "error: invalid float, expected `inf`\n --> {}:9:7\n",
                path.display()
            )
        };
        let finish = "{\"reason\":\"build-finished\",\"success\":true}";
        let evaluate = |ok: bool, path: &Path, tail: &str| {
            let run = exec::Run {
                case_observations: None,
                stopped_at: None,
                hung: None,
                ok,
                code: Some(if ok { 0 } else { 101 }),
                spawn_error: None,
                usage: None,
                output: format!("{}{artifact}\n{tail}\n", diagnostic(path)),
            };
            let mut red = Red::default();
            let read = read_cargo_run_with_discovery(
                &run,
                "aarch64-unknown-linux-gnu",
                &graph,
                false,
                &mut red,
                Some(&context),
            )
            .unwrap();
            (read, red)
        };
        let (read, red) = evaluate(true, &unused, finish);
        assert_eq!(read.checked, graph);
        assert!(red.status.is_none());
        assert_eq!(red.discovery.len(), 1);
        assert!(red.discovery[0].contains("fixture/Cargo.toml"));
        for (ok, path, tail) in [
            (false, &unused, finish),
            (true, &selected, finish),
            (true, &inherited, finish),
            (true, &outside, finish),
            (true, &unused, ""),
            (
                true,
                &unused,
                "{\"reason\":\"build-finished\",\"success\":false}",
            ),
        ] {
            assert!(evaluate(ok, path, tail).1.status.is_some());
        }
        let compiler_error = format!(
            "{finish}\n{}",
            r#"{"reason":"compiler-message","package_id":"path+file:///r/app#app@1","message":{"level":"error","rendered":"error[E0001]: real error"}}"#
        );
        assert!(evaluate(true, &unused, &compiler_error).1.status.is_some());
    }

    #[test]
    fn member_names_come_out_of_cargo_metadata() {
        let cells = aterm_forge::resolve::default_cells();
        let json =
            r#"{"packages":[{"name":"aterm","metadata":null},{"name":"arrayvec"}],"version":1}"#;
        let members = parse_members(json, &cells).expect("names");
        assert_eq!(members.names, names(&["aterm", "arrayvec"]));
        assert!(members.roots.is_empty());
        assert!(parse_members("not json", &cells).is_err());
        assert!(parse_members(r#"{"packages":[]}"#, &cells).is_err());
    }

    /// A member names the cells that check it beside their root; a name that
    /// is no cell, or a key that is no list, is refused rather than dropped.
    #[test]
    fn a_member_names_the_cells_that_check_it_beside_their_root() {
        let cells = aterm_forge::resolve::default_cells();
        let cell = |name: &str| cells.iter().find(|c| c.name == name).unwrap().clone();
        let json = r#"{"packages":[
            {"name":"aterm","metadata":null},
            {"name":"guest","metadata":{"aterm":{"cells":["linux-arm"]}}},
            {"name":"both","metadata":{"aterm":{"cells":["linux-arm","win"]}}},
            {"name":"docs-only","metadata":{"docs":{"rs":{}}}}
        ]}"#;
        let members = parse_members(json, &cells).expect("members");
        assert_eq!(members.roots_of(&cell("linux-arm")), ["both", "guest"]);
        assert_eq!(members.roots_of(&cell("win")), ["both"]);
        assert_eq!(
            members.roots_of(&cell("win-arm")),
            Vec::<String>::new(),
            "a cell no member names checks its root alone"
        );
        assert_eq!(
            lint_packages(&cell("linux-arm"), &members.roots_of(&cell("linux-arm"))),
            ["-p", "aterm", "-p", "both", "-p", "guest"]
        );
        assert_eq!(lint_packages(&cell("win-arm"), &[]), ["-p", "aterm"]);
        let unknown =
            r#"{"packages":[{"name":"g","metadata":{"aterm":{"cells":["linux-musl"]}}}]}"#;
        let why = parse_members(unknown, &cells).unwrap_err();
        assert!(why.contains("`g`") && why.contains("linux-musl"), "{why}");
        let not_a_list =
            r#"{"packages":[{"name":"g","metadata":{"aterm":{"cells":"linux-arm"}}}]}"#;
        assert!(parse_members(not_a_list, &cells).is_err());
    }

    /// An extra root's graph is joined onto the cell's: the test pass selects it and its own
    /// members, the host side stays empty, and a cell no member names still excludes it.
    #[test]
    fn an_extra_root_joins_the_cells_graph_and_its_test_pass() {
        let dir = aterm_tempfile::tempdir().expect("private resolver fixture");
        let root = dir.path();
        let packages = [
            ("app", "[dependencies]\nshared={path='../shared'}\n"),
            (
                "guest",
                "[dependencies]\nshared={path='../shared'}\nguest-only={path='../guest-only'}\n\
                 [package.metadata.aterm]\ncells=['linux-arm']\n",
            ),
            ("shared", ""),
            ("guest-only", ""),
            ("mac-only", ""),
        ];
        let listed: Vec<&str> = packages.iter().map(|(n, _)| *n).collect();
        std::fs::write(
            root.join("Cargo.toml"),
            format!("[workspace]\nresolver='2'\nmembers={listed:?}\n"),
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
             [[package]]\nname='app'\nversion='1.0.0'\ndependencies=['shared']\n\
             [[package]]\nname='guest'\nversion='1.0.0'\ndependencies=['guest-only','shared']\n\
             [[package]]\nname='guest-only'\nversion='1.0.0'\n\
             [[package]]\nname='mac-only'\nversion='1.0.0'\n\
             [[package]]\nname='shared'\nversion='1.0.0'\n",
        )
        .expect("locked local dependency graph");
        let targo = PathBuf::from(env!("CARGO"));
        let path = std::env::var_os("PATH").unwrap_or_default();
        let metadata = aterm_uds::spawnfd::command(&targo)
            .args([
                "metadata",
                "--no-deps",
                "--locked",
                "--offline",
                "--format-version",
                "1",
            ])
            .arg("--manifest-path")
            .arg(root.join("Cargo.toml"))
            .current_dir(root)
            .env("PATH", &path)
            .output_owned()
            .expect("targo metadata");
        assert!(metadata.status.success(), "{metadata:?}");
        let cells = aterm_forge::resolve::default_cells();
        let members = parse_members(&String::from_utf8_lossy(&metadata.stdout), &cells)
            .expect("the fixture's members");
        let cargo = aterm_forge::resolve::Cargo::pinned(&targo, &path);
        let resolve = |name: &str, package: &str| {
            let cell = Cell {
                package: package.to_string(),
                ..cells.iter().find(|c| c.name == name).unwrap().clone()
            };
            let extra = members.roots_of(&cell);
            let mut log = String::new();
            let (graph, _) = resolve_roots(&cargo, root, &cell, &extra, &mut log)
                .expect("resolve the fixture without compiling");
            (extra, graph)
        };

        let (extra, graph) = resolve("linux-arm", "app");
        assert_eq!(extra, ["guest"]);
        let normal: BTreeSet<String> = graph.nodes.iter().map(|id| id.name.clone()).collect();
        assert_eq!(normal, names(&["app", "guest", "guest-only", "shared"]));
        assert_eq!(host_side(&graph, |_| false), BTreeSet::new());
        let (selected, excluded) = test_selection(&members.names, &normal, &BTreeSet::new());
        assert_eq!(selected, ["app", "guest", "guest-only", "shared"]);
        assert_eq!(excluded, ["mac-only"]);

        // Control: a cell the guest does not name leaves it out.
        let (extra, graph) = resolve("win-arm", "app");
        assert!(extra.is_empty());
        let normal: BTreeSet<String> = graph.nodes.iter().map(|id| id.name.clone()).collect();
        assert_eq!(normal, names(&["app", "shared"]));
        let (_, excluded) = test_selection(&members.names, &normal, &BTreeSet::new());
        assert_eq!(excluded, ["guest", "guest-only", "mac-only"]);
    }

    /// The members the graph carries are selected; the rest are excluded,
    /// except a patched one, whose exclusion would drop its patch.
    #[test]
    fn the_test_pass_selects_the_graph_and_never_excludes_a_patch() {
        let members = names(&["a", "b", "arrayvec", "z"]);
        let graph = names(&["a", "b", "serde"]);
        let (selected, excluded) = test_selection(&members, &graph, &names(&["arrayvec"]));
        assert_eq!(selected, ["a", "b"]);
        assert_eq!(excluded, ["z"]);
    }

    /// A crate only a proc macro reaches compiles for the host with it and is
    /// not owed a target artifact; one a non-proc-macro path also reaches is.
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
    /// the lane's jobs and never above the caller's own `CARGO_BUILD_JOBS`, and
    /// keeps no incremental state.
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
                name: "1.97.1".into(),
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
            let packages = ["-p".to_string(), "aterm-agent".to_string()];
            let narrowed = cargo.selected_targets(&packages);
            assert!(
                narrowed.args.iter().any(|a| a == "--all-targets")
                    && narrowed.args.iter().any(|a| a == "aterm-agent")
                    && !narrowed.args.iter().any(|a| a == "--workspace"),
                "{:?}",
                narrowed.args
            );
            // The lint compiles what ships, with debug assertions off, and the pass over test
            // targets, which call the seams they gate, keeps them on.
            let release = |cmd: &Cmd| cmd.args.iter().any(|a| a == "--release");
            assert!(release(&cargo.lint(&packages)), "the lint is debug-off");
            assert!(!release(&narrowed), "CONTROL: test targets are debug-on");
            let cmd = cargo.all_targets(&[]);
            let get = |k: &str| {
                cmd.envs
                    .iter()
                    .find(|(name, _)| name == k)
                    .map(|(_, v)| v.to_string_lossy().into_owned())
            };
            assert_eq!(get("CARGO_INCREMENTAL").as_deref(), Some("0"));
            (get("CARGO_TARGET_DIR"), get("CARGO_BUILD_JOBS"))
        };
        let dir = Some(
            ["/repo", "target", "verify", ".ubd", "cells.noindex", "win"]
                .iter()
                .collect::<PathBuf>()
                .to_string_lossy()
                .into_owned(),
        );
        assert_eq!(env_of(None), (dir.clone(), Some("4".to_string())));
        assert_eq!(env_of(Some("2")), (dir.clone(), Some("2".to_string())));
        assert_eq!(env_of(Some("16")), (dir, Some("4".to_string())));
    }

    /// Either Windows cell brings both windows-sys oracles, once each; a run
    /// with no Windows cell runs neither.
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
        assert!(
            runs_win32_oracles(&foreign_cells("aarch64-apple-darwin")),
            "the default run"
        );
        assert_eq!(
            WIN32_ORACLES.map(|(verb, _)| verb),
            ["oracle", "oracle-052"]
        );

        let root = std::path::absolute("/repo").expect("absolute");
        let ctx = Ctx::new(
            root.clone(),
            crate::Scope::workspace(),
            crate::EnvSnapshot::default(),
            PathBuf::from("/tmp"),
        );
        let cmd = win32_oracle_cmd(&ctx, "oracle-052", "1.97.1").expect("absolute paths");
        let argv = cmd.argv();
        let oracle_bin = root
            .join("target")
            .join("verify")
            .join(".ubd")
            .join("cells.noindex")
            .join("win32-oracle-bin")
            .display()
            .to_string();
        let root_str = root.display().to_string();
        assert_eq!(
            argv[1..],
            [
                "--unverified",
                "run",
                "-q",
                "-p",
                "aterm-win32-oracle",
                "--target-dir",
                oracle_bin.as_str(),
                "--",
                "oracle-052",
                "--root",
                root_str.as_str(),
                "--stock",
                "1.97.1",
            ]
        );
        assert_eq!(cmd.cwd.as_deref(), Some(root.as_path()));
        assert!(
            !cmd.envs.iter().any(|(k, _)| k == "CARGO_TARGET_DIR"),
            "the oracle's own Windows builds keep their per-checkout directories"
        );
    }

    /// An oracle that decided nothing says why on its last `could not run`
    /// line, which becomes the row's reason; a finding's output has none.
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

    #[test]
    fn patched_crates_and_renamed_package_handle_subtables_quotes_and_comments() {
        assert_eq!(
            renamed_package("{ path = \"crates/package = 'bad'\", package = 'real-pkg' }"),
            Some("real-pkg")
        );
        assert_eq!(
            renamed_package("{ my_package = \"wrong\" } # package = \"comment\""),
            None
        );
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[patch.'crates-io'] # inline table\n\
             foo = { path = \"crates/foo\" }\n\
             alias = { path = \"crates/bar\", package = 'bar-real' }\n\
             [patch.crates-io.sub]\n\
             path = \"crates/sub\"\n\
             [patch.\"crates-io\".sub_alias]\n\
             path = \"crates/sub2\"\n\
             package = \"sub-real\"\n",
        )
        .expect("write");
        assert_eq!(
            patched_crates(dir.path()).expect("patched"),
            names(&["foo", "bar-real", "sub", "sub-real"])
        );
    }
}
