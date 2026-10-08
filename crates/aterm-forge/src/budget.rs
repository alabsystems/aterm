// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `tools/forge-budget.tsv`: the ratchet. The third-party surface may only
//! shrink, and every exception is recorded in the file.
//!
//! # The file
//!
//! Shaped like `tools/trust-gate-ratchet.tsv`: tab-separated, no header, one
//! fact per line, readable with `awk -F'\t'`.
//!
//! ```text
//! <scope>\t<metric>\t<ceiling>[\t<regress-reason>]
//! ```
//!
//! No comments and no blank-line grammar: a `#` line is refused by line number,
//! so rows cannot be commented out.
//!
//! # The rule
//!
//! * `live < ceiling` → green with the slack named. `--update` lowers the
//!   ceiling to the live value; the only edit forge makes on its own.
//! * `live == ceiling` → green, at ceiling.
//! * `live > ceiling` → red. Raising needs `--update --allow-regress "<reason>"`
//!   with at least [`MIN_REASON_CHARS`] characters; the reason is written as
//!   the 4th column, and every run counts raised ceilings and points at the
//!   file.
//!
//! # Build scripts and proc macros are rows
//!
//! A build script is code the compiler executes, and `targo trust` marks every
//! one `-Ztrust-verify=off`. While any third-party build script remains, no
//! amount of source deletion retires that opt-out, so build scripts and proc
//! macros have their own ceilings beside LOC.
//!
//! # Not seeded
//!
//! `shipped.<triple> packages` and `lock packages` are supported metrics but
//! the seeded rows (`--update` into an empty checkout, `seed_shape`) do not arm
//! them: both count aterm's own crates, so a new first-party crate would demand
//! a justification for growth that is not third-party surface. They can be
//! added by hand.

use crate::Outcome;
use crate::model::{Cell, CellSurvey};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

/// The ratchet's path, relative to the workspace root.
pub const BUDGET_PATH: &str = "tools/forge-budget.tsv";

/// The minimum length of a `--allow-regress` reason, in characters: enough for
/// a sentence saying what grew, why, and what would shrink it.
pub const MIN_REASON_CHARS: usize = 80;

/// Metrics measured per `shipped.<triple>` scope.
const SHIPPED_METRICS: &[&str] = &[
    "packages",
    "third_party_packages",
    "third_party_loc",
    "build_scripts",
    "proc_macros",
    "duplicate_names",
];
/// Metrics measured for the `lock` scope (the whole `Cargo.lock`, every target).
const LOCK_METRICS: &[&str] = &["packages", "third_party_packages"];
/// Metrics measured for the `patch` scope: the vendored arm of
/// `[patch.crates-io]`.
///
/// A first-party patch target is aterm's own crate and is not counted here
/// (the same reason `packages` rows are unseeded). It is still held to
/// existing and being live by `forge attest` [OB-1]/[OB-2] and `forge check`
/// [OB-12], and a not-live note is emitted below for every patch entry.
const PATCH_METRICS: &[&str] = &["entries", "live_entries"];

/// One ratchet row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// `shipped.<triple>`, `lock`, or `patch`.
    pub scope: String,
    pub metric: String,
    /// The recorded high-water mark. Live may equal it; never exceed it.
    pub ceiling: u64,
    /// The accepted `--allow-regress` prose, recorded in the file's 4th column.
    pub regress_reason: Option<String>,
}

impl Row {
    /// The row's TSV line, without the newline.
    pub fn line(&self) -> String {
        match &self.regress_reason {
            Some(r) => format!("{}\t{}\t{}\t{}", self.scope, self.metric, self.ceiling, r),
            None => format!("{}\t{}\t{}", self.scope, self.metric, self.ceiling),
        }
    }
}

// ---------------------------------------------------------------------------
// Reading and writing the file
// ---------------------------------------------------------------------------

/// Read `<root>/tools/forge-budget.tsv`. An absent file is an empty `Vec`;
/// [`run`] refuses to call an unarmed ratchet a pass.
pub fn load(root: &Path) -> Result<Vec<Row>, String> {
    match std::fs::read_to_string(root.join(BUDGET_PATH)) {
        Ok(text) => parse(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!(
            "cannot read {} ({e}) — fix the permissions, or delete the file and re-seed it \
             with `targo --unverified forge budget --update`",
            root.join(BUDGET_PATH).display()
        )),
    }
}

/// Parse a ratchet body. Every refusal names the line number, because a
/// one-fact-per-line file's only useful coordinate is the line.
pub fn parse(text: &str) -> Result<Vec<Row>, String> {
    let mut rows: Vec<Row> = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let n = i + 1;
        if raw.trim().is_empty() {
            continue;
        }
        if raw.starts_with('#') {
            return Err(bad(
                n,
                raw,
                &format!(
                    "comments are not supported. This file is pure TSV so one `awk -F'\\t'` idiom \
                 reads it and {BUDGET_PATH}'s sibling tools/trust-gate-ratchet.tsv. Delete the \
                 line; the prose belongs in aterm_forge::budget's module docs"
                ),
            ));
        }
        let f: Vec<&str> = raw.split('\t').collect();
        if f.len() == 1 && raw.split_whitespace().count() >= 3 {
            return Err(bad(n, raw, "the columns are separated by TABS, not spaces"));
        }
        if f.len() < 3 || f.len() > 4 {
            return Err(bad(
                n,
                raw,
                &format!("expected 3 or 4 tab-separated columns, found {}", f.len()),
            ));
        }
        let (scope, metric, ceiling) = (f[0].trim(), f[1].trim(), f[2].trim());
        if scope.is_empty() || metric.is_empty() {
            return Err(bad(
                n,
                raw,
                "the scope and metric columns must both be non-empty",
            ));
        }
        let ceiling: u64 = ceiling
            .parse()
            .map_err(|_| bad(n, raw, &format!("`{ceiling}` is not a whole number")))?;
        let regress_reason = match f.get(3) {
            None => None,
            Some(r) if r.trim().is_empty() => {
                return Err(bad(
                    n,
                    raw,
                    &format!(
                        "the 4th column is empty. It holds an accepted --allow-regress reason of \
                     at least {MIN_REASON_CHARS} characters; delete the trailing tab if there \
                     is no regression to explain"
                    ),
                ));
            }
            Some(r) if r.chars().count() < MIN_REASON_CHARS => {
                return Err(bad(
                    n,
                    raw,
                    &format!(
                        "the recorded regress reason is {} characters; a raise needs at least \
                     {MIN_REASON_CHARS}. Write what grew, why it had to, and what would shrink \
                     it again",
                        r.chars().count()
                    ),
                ));
            }
            Some(r) => Some((*r).to_string()),
        };
        if let Some(prev) = rows.iter().find(|p| p.scope == scope && p.metric == metric) {
            return Err(bad(
                n,
                raw,
                &format!(
                    "`{} {}` already has a ceiling of {} earlier in this file — one row per \
                 scope+metric",
                    prev.scope, prev.metric, prev.ceiling
                ),
            ));
        }
        rows.push(Row {
            scope: scope.to_string(),
            metric: metric.to_string(),
            ceiling,
            regress_reason,
        });
    }
    Ok(rows)
}

fn bad(line: usize, raw: &str, why: &str) -> String {
    format!(
        "{BUDGET_PATH}:{line}: {why}\n  the line reads: {raw:?}\n  expected: \
         <scope>\\t<metric>\\t<ceiling>[\\t<regress-reason>]\n  for example: \
         shipped.aarch64-apple-darwin\\tthird_party_loc\\t2130888"
    )
}

/// The file body for a set of rows, in the given order.
pub fn render(rows: &[Row]) -> String {
    let mut s = String::new();
    for r in rows {
        s.push_str(&r.line());
        s.push('\n');
    }
    s
}

/// The armed rows, in file order: the third-party facts per cell, then the lock
/// and patch facts.
fn seed_shape(live: &Live) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for scope in live.scopes.iter().filter(|s| s.starts_with("shipped.")) {
        for m in SHIPPED_METRICS.iter().filter(|m| **m != "packages") {
            out.push((scope.clone(), (*m).to_string()));
        }
    }
    out.push(("lock".to_string(), "third_party_packages".to_string()));
    out.push(("patch".to_string(), "entries".to_string()));
    out.push(("patch".to_string(), "live_entries".to_string()));
    out
}

// ---------------------------------------------------------------------------
// Measuring the live surface
// ---------------------------------------------------------------------------

/// Extra per-scope facts that make a red row actionable: forge cannot name
/// what is new (it does not record the package set), but it can name what the
/// mass is made of.
#[derive(Clone, Debug, Default)]
struct ScopeDetail {
    duplicates: Vec<String>,
    build_scripts: Vec<String>,
    proc_macros: Vec<String>,
    biggest: Vec<(String, u64)>,
    /// Third-party packages whose LOC was read from an in-tree fork (the source
    /// that compiles), with the directory measured.
    ///
    /// A red `third_party_loc` row may be caused by aterm's own fork edits;
    /// naming them keeps that from reading as dependency drift and being
    /// recorded with `--allow-regress`.
    vendored_measured: Vec<String>,
}

#[derive(Clone, Debug, Default)]
struct Live {
    values: BTreeMap<(String, String), u64>,
    /// Scopes in measurement order, for stable seeding.
    scopes: Vec<String>,
    /// Scope → why it could not be measured. Never silently absent.
    unavailable: BTreeMap<String, String>,
    detail: BTreeMap<String, ScopeDetail>,
    /// Free-text observations printed under the table (patch liveness, …).
    notes: Vec<String>,
}

impl Live {
    fn set(&mut self, scope: &str, metric: &str, value: u64) {
        self.values
            .insert((scope.to_string(), metric.to_string()), value);
    }
    fn get(&self, scope: &str, metric: &str) -> Option<u64> {
        self.values
            .get(&(scope.to_string(), metric.to_string()))
            .copied()
    }
}

fn u(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

/// The ratchet scope one cell's rows live under.
///
/// A scope names exactly one cell (it is the file's primary key). Native cells
/// use `shipped.<triple>`. `wasm32-unknown-unknown` carries two cells (two
/// independent browser modules, see `resolve::default_cells`), so those append
/// the cell handle: `shipped.wasm32-unknown-unknown.wasm-gpu`.
///
/// If a shared triple drops to one cell, the other's rows are not renamed:
/// they name a scope nothing measures, and [`validate_metric`] makes that a
/// could-not-run.
fn scope_of(cell: &Cell, cells: &[Cell]) -> String {
    if cells.iter().filter(|c| c.triple == cell.triple).count() > 1 {
        format!("shipped.{}.{}", cell.triple, cell.name)
    } else {
        format!("shipped.{}", cell.triple)
    }
}

fn measure(root: &Path) -> Result<Live, String> {
    let mut live = Live::default();

    let cells = crate::resolve::default_cells();
    for cell in &cells {
        let scope = scope_of(cell, &cells);
        live.scopes.push(scope.clone());
        match crate::loc::survey_cell(root, cell) {
            Ok(s) => {
                live.set(&scope, "packages", u(s.graph.nodes.len()));
                live.set(&scope, "third_party_packages", u(s.third_party().count()));
                live.set(&scope, "third_party_loc", s.third_party_loc());
                live.set(&scope, "build_scripts", u(s.build_scripts()));
                live.set(&scope, "proc_macros", u(s.proc_macros()));
                live.set(&scope, "duplicate_names", u(s.duplicate_names().len()));
                live.detail.insert(scope.clone(), detail(root, &s));
            }
            // A cell that cannot resolve is NAMED, and every row scoped to it
            // reads UNMEASURED. It is never a pass.
            Err(e) => {
                live.unavailable.insert(scope.clone(), e);
            }
        }
    }

    let lock = crate::policy::lock_entries(root)?;
    let patches = crate::policy::patch_entries(root)?;
    // A lock entry with no `source` is a path package, one of three kinds:
    //
    //   * an ordinary workspace member                     -> aterm's own
    //   * a VENDORED `[patch]` fork under vendor/          -> THIRD-PARTY
    //   * a FIRST-PARTY `[patch]` replacement under crates/ -> aterm's own
    //
    // A vendored fork is upstream source aterm maintains and reviews, so it
    // counts as third-party here as in a shipped cell.
    let vendored_patch_names: Vec<&str> = patches
        .iter()
        .filter(|p| p.is_vendored())
        .map(|p| p.name.as_str())
        .collect();
    let workspace = lock
        .iter()
        .filter(|e| e.source.is_none() && !vendored_patch_names.contains(&e.name.as_str()))
        .count();
    live.scopes.push("lock".to_string());
    live.set("lock", "packages", u(lock.len()));
    live.set(
        "lock",
        "third_party_packages",
        u(lock.len().saturating_sub(workspace)),
    );

    live.scopes.push("patch".to_string());
    let vendored: Vec<&crate::policy::PatchEntry> =
        patches.iter().filter(|p| p.is_vendored()).collect();
    live.set("patch", "entries", u(vendored.len()));
    live.set(
        "patch",
        "live_entries",
        u(vendored.iter().filter(|p| p.is_live()).count()),
    );
    for p in patches.iter().filter(|p| !p.is_vendored()) {
        live.notes.push(format!(
            "FIRST-PARTY PATCH (not counted in the `patch` ratchet, which bounds third-party \
             fork surface): {} → {} at {}, {}. It is aterm's own crate; `targo --unverified forge attest` \
             [OB-1]/[OB-2] and `targo --unverified forge check` [OB-12] are what hold it to being present \
             and live.",
            p.name,
            p.path,
            p.manifest_version,
            if p.is_live() { "LIVE" } else { "NOT LIVE" }
        ));
    }
    // Liveness notes below cover EVERY entry: a dead patch compiles into
    // nothing whoever wrote it.
    for p in &patches {
        if !p.is_live() {
            live.notes.push(format!(
                "PATCH NOT LIVE: {} is vendored at {} but the lock resolved {:?} — the fix in \
                 {} compiles into nothing (`targo --unverified forge attest` has the repair)",
                p.name, p.manifest_version, p.lock_version, p.path
            ));
        }
        if !p.shadowed_by.is_empty() {
            live.notes.push(format!(
                "PATCH SHADOWED: {} is forked at {} and the lock ALSO carries registry {} {} — \
                 the fix is not everywhere the name is",
                p.name,
                p.manifest_version,
                p.name,
                p.shadowed_by.join(", ")
            ));
        }
    }

    Ok(live)
}

fn detail(root: &Path, s: &CellSurvey) -> ScopeDetail {
    let vendor = root.join("vendor");
    let mut biggest: Vec<(String, u64)> = s
        .third_party()
        .filter_map(|p| s.facts.get(p).map(|f| (p.spec(), f.loc)))
        .collect();
    biggest.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    biggest.truncate(5);
    ScopeDetail {
        duplicates: s
            .duplicate_names()
            .into_iter()
            .map(|(n, v)| format!("{n} ({})", v.join(", ")))
            .collect(),
        build_scripts: s
            .third_party()
            .filter(|p| s.facts.get(*p).is_some_and(|f| f.has_build_rs))
            .map(crate::model::PkgId::spec)
            .collect(),
        proc_macros: s
            .third_party()
            .filter(|p| s.facts.get(*p).is_some_and(|f| f.is_proc_macro))
            .map(crate::model::PkgId::spec)
            .collect(),
        biggest,
        vendored_measured: s
            .third_party()
            .filter_map(|p| s.facts.get(p).map(|f| (p, f)))
            .filter(|(_, f)| {
                f.root_dir
                    .as_ref()
                    .is_some_and(|dir| dir.starts_with(&vendor))
            })
            // The directory actually measured, not `vendor/<name>`
            // (`vendor/astream` holds several crates under `crates/<name>`).
            .map(|(p, f)| {
                let shown = f
                    .root_dir
                    .as_ref()
                    .and_then(|d| d.strip_prefix(root).ok())
                    .map(|d| d.display().to_string())
                    .unwrap_or_else(|| format!("vendor/{}", p.name));
                format!("{} ({shown})", p.spec())
            })
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// The ratchet decision (pure, so the rules are testable without a graph)
// ---------------------------------------------------------------------------

/// What one row's comparison came to.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Verdict {
    /// Live is below the ceiling by `slack`.
    Under(u64),
    At,
    /// Live is above the ceiling by `over`.
    Over(u64),
    /// The scope could not be measured — never a pass.
    Unmeasured,
}

/// A validated `--allow-regress` reason, or the refusal that names the fix.
fn validate_reason(update: bool, reason: Option<&str>) -> Result<Option<String>, String> {
    let Some(r) = reason else { return Ok(None) };
    if !update {
        return Err(format!(
            "`--allow-regress` was given without `--update`, so nothing would be written. \
             Run: targo --unverified forge budget --update --allow-regress \"{r}\""
        ));
    }
    let len = r.chars().count();
    if len < MIN_REASON_CHARS {
        return Err(format!(
            "the --allow-regress reason is {len} characters; raising a ceiling needs at least \
             {MIN_REASON_CHARS}. It is written into {BUDGET_PATH} for good, so write the \
             sentence that will still make sense in a year: what grew, why it had to, and what \
             would shrink it again"
        ));
    }
    if r.contains('\t') || r.contains('\n') {
        return Err(format!(
            "the --allow-regress reason contains a tab or newline, and it is stored as the \
             4th TAB-SEPARATED column of {BUDGET_PATH} — write it as one line of prose"
        ));
    }
    Ok(Some(r.to_string()))
}

fn verdict(ceiling: u64, live: Option<u64>) -> Verdict {
    match live {
        None => Verdict::Unmeasured,
        Some(v) if v < ceiling => Verdict::Under(ceiling - v),
        Some(v) if v == ceiling => Verdict::At,
        Some(v) => Verdict::Over(v - ceiling),
    }
}

/// The outcome of applying the ratchet rules to every row.
struct Plan {
    rows: Vec<Row>,
    changed: bool,
    ok: bool,
    /// How many ceilings this run RAISED. More than one means a single
    /// `--allow-regress` sentence was recorded against several rows.
    raised: usize,
    table: Vec<[String; 5]>,
    problems: Vec<String>,
}

fn plan(rows: &[Row], live: &Live, update: bool, reason: Option<&str>) -> Plan {
    let mut out = Plan {
        rows: rows.to_vec(),
        changed: false,
        ok: true,
        raised: 0,
        table: Vec::new(),
        problems: Vec::new(),
    };
    for row in &mut out.rows {
        let value = live.get(&row.scope, &row.metric);
        let v = verdict(row.ceiling, value);
        let shown = value.map_or_else(|| "?".to_string(), |v| v.to_string());
        let note = match &v {
            Verdict::Unmeasured => {
                out.ok = false;
                let why = live
                    .unavailable
                    .get(&row.scope)
                    .cloned()
                    .unwrap_or_else(|| "the scope was not measured".to_string());
                out.problems.push(format!(
                    "UNMEASURED  {} {} — {why}\n      A ratchet row forge cannot measure is \
                     not a row that passed. Fix the cell (it resolves offline: no toolchain \
                     for that target is needed), or delete the row.",
                    row.scope, row.metric
                ));
                "UNMEASURED".to_string()
            }
            Verdict::At => "GREEN (at ceiling)".to_string(),
            Verdict::Under(slack) => {
                if update {
                    let was = row.ceiling;
                    row.ceiling = value.unwrap_or(row.ceiling);
                    out.changed = true;
                    format!("GREEN  LOWERED {was} -> {}", row.ceiling)
                } else {
                    format!("GREEN (slack {slack} — `--update` locks it in)")
                }
            }
            Verdict::Over(over) => match (update, reason) {
                (true, Some(r)) => {
                    let was = row.ceiling;
                    row.ceiling = value.unwrap_or(row.ceiling);
                    row.regress_reason = Some(r.to_string());
                    out.changed = true;
                    out.raised += 1;
                    format!("RAISED {was} -> {} (+{over}, recorded)", row.ceiling)
                }
                _ => {
                    out.ok = false;
                    out.problems.push(over_message(row, *over, live));
                    format!("RED (+{over} over)")
                }
            },
        };
        out.table.push([
            row.scope.clone(),
            row.metric.clone(),
            row.ceiling.to_string(),
            shown,
            note,
        ]);
    }
    out
}

/// The RED diagnostic for one row: what it costs, what forge can and cannot
/// say about why, and the two ways out.
fn over_message(row: &Row, over: u64, live: &Live) -> String {
    let mut s = format!(
        "RED  {} {} is {over} over the ceiling of {}.\n",
        row.scope, row.metric, row.ceiling
    );
    if let Some(d) = live.detail.get(&row.scope) {
        let listed: Option<(&str, String)> = match row.metric.as_str() {
            "duplicate_names" => Some(("the duplicated names now", d.duplicates.join(", "))),
            "build_scripts" => Some(("the build scripts now", d.build_scripts.join(", "))),
            "proc_macros" => Some(("the proc macros now", d.proc_macros.join(", "))),
            "third_party_loc" | "third_party_packages" => Some((
                "the five biggest third-party packages now",
                d.biggest
                    .iter()
                    .map(|(n, l)| format!("{n} ({l} LOC)"))
                    .collect::<Vec<_>>()
                    .join(", "),
            )),
            _ => None,
        };
        if let Some((label, list)) = listed
            && !list.is_empty()
        {
            let _ = writeln!(s, "      {label}: {list}");
        }
    }
    s.push_str(
        "      forge does not record the package SET a ceiling was taken over, so it cannot \
         name what is NEW. `git diff -- Cargo.lock | grep '^+name'` does, and `targo --unverified forge \
         survey --cell <name>` ranks what it costs by dominator.\n",
    );
    // For `third_party_loc`, name the packages measured from in-tree forks:
    // fork edits move only the line total, never the counts.
    if row.metric == "third_party_loc"
        && let Some(d) = live.detail.get(&row.scope)
        && !d.vendored_measured.is_empty()
    {
        let _ = writeln!(
            s,
            "      MEASURED FROM RESOLVED VENDORED SOURCE: {}. `loc::package_dir` reads a \
             [patch.crates-io] target's actual Cargo path before consulting the registry; a \
             direct-path dependency has no branch of its own and is reached LAST, through the \
             cargo-tree hint, so it is measured from its real directory only because an \
             unpublished crate has no registry copy to shadow it. These are the complete \
             retained package sources, including tests and \
             examples, not only local edits or reachable code. `targo --unverified forge attest` checks \
             fork obligations and the astream submodule's gitlink pin.",
            d.vendored_measured.join(", ")
        );
    }
    let _ = write!(
        s,
        "      Either shrink it back, or record the regression:\n        targo --unverified forge budget \
         --update --allow-regress \"<at least {MIN_REASON_CHARS} characters: what grew, why it \
         had to, what would shrink it again>\"",
    );
    s
}

// ---------------------------------------------------------------------------
// The verb
// ---------------------------------------------------------------------------

/// Compare the live surface against `tools/forge-budget.tsv`.
pub fn run(root: &Path, update: bool, allow_regress: Option<&str>) -> Result<Outcome, String> {
    // Refuse a bad reason BEFORE spending seconds on the survey: an argument
    // error should come back immediately, not after the work it invalidates.
    let reason = match validate_reason(update, allow_regress) {
        Ok(r) => r,
        Err(msg) => {
            return Ok(Outcome {
                ok: false,
                log: format!("cargo forge budget — REFUSED\n\n    {msg}\n"),
            });
        }
    };

    let rows = load(root)?;
    let live = measure(root)?;

    if rows.is_empty() {
        return Ok(unarmed(root, &live, update));
    }
    for row in &rows {
        validate_metric(row, &live)?;
    }

    let plan = plan(&rows, &live, update, reason.as_deref());
    let mut log = String::new();
    let _ = writeln!(
        log,
        "cargo forge budget — {BUDGET_PATH} ({} rows)\n",
        plan.rows.len()
    );
    log.push_str(&table(&plan.table));

    let unratcheted = unratcheted(&rows, &live);
    if !unratcheted.is_empty() {
        log.push_str("\n    UNRATCHETED (measured, no ceiling recorded — add a row to arm it):\n");
        for (scope, metric, value) in &unratcheted {
            let _ = writeln!(log, "      {scope}\t{metric}\t{value}");
        }
    }

    if plan.raised > 1 {
        let _ = writeln!(
            log,
            "\n    NOTE: {} ceilings were raised in this run, and the command line carries \
             ONE reason — the same sentence is now recorded against all {} of them. If they \
             grew for different reasons, raise them one at a time and edit {BUDGET_PATH}'s \
             4th column so each row carries its own.",
            plan.raised, plan.raised
        );
    }

    // A count and the file, not the reasons: 21 of them came to 55 KB, and the
    // gate printed every byte on every merge-contract run.
    let recorded = plan
        .rows
        .iter()
        .filter(|r| r.regress_reason.is_some())
        .count();
    if recorded > 0 {
        let _ = writeln!(
            log,
            "\n    {recorded} ceiling(s) carry a recorded raise — reasons in {BUDGET_PATH}."
        );
    }

    if !live.notes.is_empty() {
        log.push('\n');
        for n in &live.notes {
            let _ = writeln!(log, "    {n}");
        }
    }

    if !plan.problems.is_empty() {
        log.push('\n');
        for p in &plan.problems {
            let _ = writeln!(log, "    {p}");
        }
    }

    // A cell that could not be resolved is NAMED even when no row depends on it:
    // a measurement forge claims to make and did not make is never a pass.
    let mut ok = plan.ok;
    if !live.unavailable.is_empty() {
        ok = false;
        log.push('\n');
        for (scope, why) in &live.unavailable {
            let _ = writeln!(
                log,
                "    COULD NOT MEASURE {scope} — {why}\n      Resolution needs no toolchain \
                 for the target, so this is a real failure, not a missing cross-compiler. \
                 Re-run `targo --unverified forge survey --cell <name>` to see it directly."
            );
        }
    }

    if plan.changed {
        write_rows(root, &plan.rows)?;
        let _ = writeln!(log, "\n    WROTE {BUDGET_PATH}");
    } else if update {
        log.push_str("\n    NO CHANGE — every ceiling already sits at the live value.\n");
    }

    if ok {
        let _ = writeln!(
            log,
            "\n    VERDICT: GREEN — {} rows, none over ceiling.",
            plan.rows.len()
        );
    } else if plan.problems.is_empty() {
        let _ = writeln!(
            log,
            "\n    VERDICT: RED — every one of the {} rows held, but {} cell(s) could not be \
             measured, so this run proves nothing about them.\n\n{}",
            plan.rows.len(),
            live.unavailable.len(),
            crate::PRECISION_NOTE
        );
    } else {
        let _ = writeln!(
            log,
            "\n    VERDICT: RED — {} of {} rows over ceiling or unmeasured.\n\n{}",
            plan.problems.len(),
            plan.rows.len(),
            crate::PRECISION_NOTE
        );
    }
    Ok(Outcome { ok, log })
}

/// The report when no ratchet file exists. NOT a pass: a ratchet that ratchets
/// nothing must never read as "the surface is fine".
fn unarmed(root: &Path, live: &Live, update: bool) -> Outcome {
    let mut log = String::new();
    let _ = writeln!(log, "cargo forge budget — {BUDGET_PATH} is not armed\n");
    let body = {
        let mut rows = Vec::new();
        for (scope, metric) in seed_shape(live) {
            if let Some(&ceiling) = live.values.get(&(scope.clone(), metric.clone())) {
                rows.push(Row {
                    scope,
                    metric,
                    ceiling,
                    regress_reason: None,
                });
            }
        }
        render(&rows)
    };
    if update && live.unavailable.is_empty() {
        match write_body(root, &body) {
            Ok(()) => {
                let _ = writeln!(
                    log,
                    "    SEEDED {BUDGET_PATH} from this measurement:\n\n{}\n    The ratchet is \
                     now armed: every one of those numbers may only go down.",
                    indent(&body)
                );
                return Outcome { ok: true, log };
            }
            Err(e) => {
                let _ = writeln!(log, "    COULD NOT WRITE: {e}");
            }
        }
    }
    for (scope, why) in &live.unavailable {
        let _ = writeln!(log, "    CELL UNMEASURED: {scope} — {why}");
    }
    let _ = writeln!(
        log,
        "    The live surface measures:\n\n{}\n    Arm it with `targo --unverified forge budget --update`, \
         which writes exactly those rows. Until then this verb ratchets nothing, so it \
         reports RED rather than a pass.",
        indent(&body)
    );
    Outcome { ok: false, log }
}

fn indent(body: &str) -> String {
    body.lines().map(|l| format!("      {l}\n")).collect()
}

/// Every measured fact with no row in the file.
fn unratcheted(rows: &[Row], live: &Live) -> Vec<(String, String, u64)> {
    live.values
        .iter()
        .filter(|((s, m), _)| !rows.iter().any(|r| &r.scope == s && &r.metric == m))
        .map(|((s, m), v)| (s.clone(), m.clone(), *v))
        .collect()
}

/// A row naming a scope or metric forge does not measure is a broken ledger:
/// could-not-run, never a pass.
fn validate_metric(row: &Row, live: &Live) -> Result<(), String> {
    let known: &[&str] = if row.scope == "lock" {
        LOCK_METRICS
    } else if row.scope == "patch" {
        PATCH_METRICS
    } else if let Some(triple) = row.scope.strip_prefix("shipped.") {
        if !live.scopes.iter().any(|s| s == &row.scope) {
            return Err(format!(
                "{BUDGET_PATH} has a row for `{}` but `{triple}` is not one of the cells forge \
                 measures ({}). Add the cell to aterm_forge::resolve::default_cells, or \
                 delete the row",
                row.scope,
                live.scopes
                    .iter()
                    .filter_map(|s| s.strip_prefix("shipped."))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        SHIPPED_METRICS
    } else {
        return Err(format!(
            "{BUDGET_PATH} has a row scoped `{}`, which forge does not measure. Scopes are \
             `shipped.<triple>`, `lock` and `patch`",
            row.scope
        ));
    };
    if !known.contains(&row.metric.as_str()) {
        return Err(format!(
            "{BUDGET_PATH} has a row for `{} {}`, which forge does not measure. `{}` measures: \
             {}",
            row.scope,
            row.metric,
            row.scope,
            known.join(", ")
        ));
    }
    Ok(())
}

fn write_rows(root: &Path, rows: &[Row]) -> Result<(), String> {
    write_body(root, &render(rows))
}

fn write_body(root: &Path, body: &str) -> Result<(), String> {
    let path = root.join(BUDGET_PATH);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {} ({e})", dir.display()))?;
    }
    std::fs::write(&path, body).map_err(|e| format!("cannot write {} ({e})", path.display()))
}

fn table(rows: &[[String; 5]]) -> String {
    let w0 = rows.iter().map(|r| r[0].len()).max().unwrap_or(5).max(5);
    let w1 = rows.iter().map(|r| r[1].len()).max().unwrap_or(6).max(6);
    let w2 = rows.iter().map(|r| r[2].len()).max().unwrap_or(7).max(7);
    let w3 = rows.iter().map(|r| r[3].len()).max().unwrap_or(4).max(4);
    let mut s = format!(
        "    {:<w0$}  {:<w1$}  {:>w2$}  {:>w3$}  {}\n",
        "SCOPE", "METRIC", "CEILING", "LIVE", "VERDICT"
    );
    for r in rows {
        let _ = writeln!(
            s,
            "    {:<w0$}  {:<w1$}  {:>w2$}  {:>w3$}  {}",
            r[0], r[1], r[2], r[3], r[4]
        );
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(scope: &str, metric: &str, ceiling: u64) -> Row {
        Row {
            scope: scope.to_string(),
            metric: metric.to_string(),
            ceiling,
            regress_reason: None,
        }
    }

    fn live_with(scope: &str, metric: &str, value: u64) -> Live {
        let mut l = Live::default();
        l.scopes.push(scope.to_string());
        l.set(scope, metric, value);
        l
    }

    const REASON_80: &str =
        "wgpu 29 pulled naga's new backend in for the WebGPU path; carve it back in v0.49";

    #[test]
    fn the_eighty_character_floor_is_exact() {
        assert_eq!(REASON_80.chars().count(), 80);
    }

    // --- parsing ------------------------------------------------------------

    #[test]
    fn a_three_column_line_parses() {
        let rows = parse("shipped.aarch64-apple-darwin\tthird_party_loc\t2130888\n").unwrap();
        assert_eq!(
            rows,
            vec![row(
                "shipped.aarch64-apple-darwin",
                "third_party_loc",
                2_130_888
            )]
        );
    }

    #[test]
    fn a_malformed_line_names_its_line_number() {
        let text = "lock\tthird_party_packages\t556\npatch\tentries\nlock\tpackages\t624\n";
        let e = parse(text).unwrap_err();
        assert!(e.starts_with(&format!("{BUDGET_PATH}:2:")), "{e}");
        assert!(e.contains("found 2"), "{e}");
        assert!(
            e.contains("<scope>"),
            "the refusal must show the shape: {e}"
        );
    }

    #[test]
    fn a_non_numeric_ceiling_names_its_line_number() {
        let e = parse("patch\tentries\tsix\n").unwrap_err();
        assert!(e.starts_with(&format!("{BUDGET_PATH}:1:")), "{e}");
        assert!(e.contains("`six` is not a whole number"), "{e}");
    }

    #[test]
    fn spaces_instead_of_tabs_are_diagnosed_as_such() {
        let e = parse("patch entries 6\n").unwrap_err();
        assert!(e.contains("TABS, not spaces"), "{e}");
    }

    #[test]
    fn a_comment_line_is_refused_by_line_number() {
        let e = parse("patch\tentries\t6\n# and here is why\n").unwrap_err();
        assert!(e.starts_with(&format!("{BUDGET_PATH}:2:")), "{e}");
        assert!(e.contains("comments are not supported"), "{e}");
    }

    #[test]
    fn a_short_recorded_reason_in_the_file_is_refused() {
        let e = parse("patch\tentries\t7\ttoo short\n").unwrap_err();
        assert!(e.contains("9 characters"), "{e}");
        assert!(e.contains(&MIN_REASON_CHARS.to_string()), "{e}");
    }

    #[test]
    fn a_duplicate_row_is_refused_by_line_number() {
        let e = parse("patch\tentries\t6\npatch\tentries\t7\n").unwrap_err();
        assert!(e.starts_with(&format!("{BUDGET_PATH}:2:")), "{e}");
    }

    #[test]
    fn a_row_with_a_reason_round_trips_through_render() {
        let text = format!("patch\tentries\t7\t{REASON_80}\n");
        let rows = parse(&text).unwrap();
        assert_eq!(rows[0].regress_reason.as_deref(), Some(REASON_80));
        assert_eq!(render(&rows), text);
    }

    #[test]
    fn an_unknown_metric_is_a_could_not_run_naming_what_is_measured() {
        let live = live_with("lock", "packages", 624);
        let e = validate_metric(&row("lock", "lines_of_code", 1), &live).unwrap_err();
        assert!(e.contains("lines_of_code"), "{e}");
        assert!(e.contains("third_party_packages"), "{e}");
        let e =
            validate_metric(&row("shipped.sparc-unknown-none", "packages", 1), &live).unwrap_err();
        assert!(e.contains("default_cells"), "{e}");
    }

    // --- the ratchet rules --------------------------------------------------

    #[test]
    fn update_lowers_a_ceiling_and_never_raises_one() {
        let live = live_with("patch", "entries", 4);
        let p = plan(&[row("patch", "entries", 6)], &live, true, None);
        assert!(p.ok);
        assert!(p.changed);
        assert_eq!(p.rows[0].ceiling, 4);
        assert!(p.table[0][4].contains("LOWERED 6 -> 4"), "{:?}", p.table[0]);
    }

    #[test]
    fn slack_is_reported_but_not_written_without_update() {
        let live = live_with("patch", "entries", 4);
        let p = plan(&[row("patch", "entries", 6)], &live, false, None);
        assert!(p.ok);
        assert!(!p.changed);
        assert_eq!(p.rows[0].ceiling, 6);
        assert!(p.table[0][4].contains("slack 2"), "{:?}", p.table[0]);
    }

    #[test]
    fn a_raise_without_a_reason_is_red_and_writes_nothing() {
        let live = live_with("patch", "entries", 9);
        let p = plan(&[row("patch", "entries", 6)], &live, true, None);
        assert!(!p.ok);
        assert!(!p.changed, "a raise must never be written without a reason");
        assert_eq!(p.rows[0].ceiling, 6);
        assert!(
            p.problems[0].contains("--allow-regress"),
            "{}",
            p.problems[0]
        );
    }

    #[test]
    fn a_seventy_nine_character_reason_is_refused_naming_the_length() {
        let short: String = REASON_80.chars().take(79).collect();
        assert_eq!(short.chars().count(), 79);
        let e = validate_reason(true, Some(&short)).unwrap_err();
        assert!(e.contains("79 characters"), "{e}");
        assert!(e.contains("at least 80"), "{e}");
    }

    #[test]
    fn an_eighty_character_reason_is_accepted_and_recorded() {
        let reason = validate_reason(true, Some(REASON_80)).unwrap();
        assert_eq!(reason.as_deref(), Some(REASON_80));
        let live = live_with("patch", "entries", 9);
        let p = plan(
            &[row("patch", "entries", 6)],
            &live,
            true,
            reason.as_deref(),
        );
        assert!(p.ok);
        assert!(p.changed);
        assert_eq!(p.rows[0].ceiling, 9);
        assert_eq!(p.rows[0].regress_reason.as_deref(), Some(REASON_80));
        assert_eq!(p.raised, 1);
        // And it survives into the file, where it stays on record.
        assert!(render(&p.rows).contains(REASON_80));
    }

    #[test]
    fn a_recorded_reason_survives_a_later_lowering() {
        let mut r = row("patch", "entries", 9);
        r.regress_reason = Some(REASON_80.to_string());
        let live = live_with("patch", "entries", 6);
        let p = plan(&[r], &live, true, None);
        assert_eq!(p.rows[0].ceiling, 6);
        assert_eq!(
            p.rows[0].regress_reason.as_deref(),
            Some(REASON_80),
            "a regression stays visible after the surface shrinks again"
        );
    }

    #[test]
    fn allow_regress_without_update_is_refused_naming_the_command() {
        let e = validate_reason(false, Some(REASON_80)).unwrap_err();
        assert!(e.contains("--update"), "{e}");
    }

    #[test]
    fn a_tab_in_the_reason_is_refused_because_the_file_is_tsv() {
        let r = format!("{REASON_80}\tand more");
        let e = validate_reason(true, Some(&r)).unwrap_err();
        assert!(e.contains("tab"), "{e}");
    }

    #[test]
    fn an_unmeasured_scope_is_never_a_pass() {
        let mut live = Live::default();
        live.unavailable.insert(
            "shipped.x86_64-pc-windows-msvc".to_string(),
            "no such target".to_string(),
        );
        let p = plan(
            &[row("shipped.x86_64-pc-windows-msvc", "third_party_loc", 10)],
            &live,
            false,
            None,
        );
        assert!(!p.ok);
        assert_eq!(p.table[0][3], "?");
        assert!(
            p.problems[0].contains("no such target"),
            "{}",
            p.problems[0]
        );
    }

    #[test]
    fn a_red_row_names_the_two_ways_out() {
        let mut live = live_with("shipped.aarch64-apple-darwin", "duplicate_names", 11);
        live.detail.insert(
            "shipped.aarch64-apple-darwin".to_string(),
            ScopeDetail {
                duplicates: vec!["hashbrown (0.15.5, 0.16.0, 0.17.1)".to_string()],
                ..ScopeDetail::default()
            },
        );
        let p = plan(
            &[row("shipped.aarch64-apple-darwin", "duplicate_names", 8)],
            &live,
            false,
            None,
        );
        assert!(!p.ok);
        let m = &p.problems[0];
        assert!(m.contains("3 over the ceiling of 8"), "{m}");
        assert!(
            m.contains("hashbrown"),
            "the RED must name what it is made of: {m}"
        );
        assert!(m.contains("git diff -- Cargo.lock"), "{m}");
        assert!(m.contains("--allow-regress"), "{m}");
    }

    /// Every vendored row names the directory actually measured, and it
    /// exists (nested crates such as `vendor/astream/crates/<name>` included).
    #[test]
    fn the_live_budget_detail_names_a_vendor_directory_that_exists() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let cell = crate::resolve::default_cells().remove(0);
        let survey = crate::loc::real_workspace_survey(&cell).expect("mac graph resolves");
        let facts = detail(root, &survey);
        assert!(
            !facts.vendored_measured.is_empty(),
            "the mac cell vendors winit, so this list is never empty"
        );
        for row in &facts.vendored_measured {
            let shown = row
                .rsplit_once(" (")
                .and_then(|(_, dir)| dir.strip_suffix(')'))
                .unwrap_or_else(|| panic!("{row}: no directory in the detail line"));
            assert!(
                shown.starts_with("vendor/"),
                "{row}: a measured fork is under vendor/"
            );
            assert!(
                root.join(shown).is_dir(),
                "{row}: `{shown}` is not a directory in this checkout"
            );
        }
        // The non-existent flat spelling a nested crate must not produce.
        assert!(
            !facts
                .vendored_measured
                .iter()
                .any(|s| s.contains("(vendor/astream-cap)")),
            "{:?}",
            facts.vendored_measured
        );
    }

    #[test]
    fn load_of_an_absent_file_is_an_empty_ratchet_not_an_error() {
        let rows = load(Path::new("/nonexistent/aterm-forge-test-root")).unwrap();
        assert!(rows.is_empty());
    }
}
