// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Third-party surface ratchet (`tools/forge-budget.tsv`).

use crate::Outcome;
use crate::model::{Cell, CellSurvey};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

/// The ratchet's path, relative to the workspace root.
pub const BUDGET_PATH: &str = "tools/forge-budget.tsv";

/// Metrics measured per `shipped.<triple>` scope.
const SHIPPED_METRICS: &[&str] = &[
    "packages",
    "third_party_packages",
    "build_scripts",
    "proc_macros",
    "duplicate_names",
];
/// Metrics measured for the `lock` scope (the whole `Cargo.lock`, every target).
const LOCK_METRICS: &[&str] = &["packages", "third_party_packages"];
/// Metrics measured for the `patch` scope.
const PATCH_METRICS: &[&str] = &["entries", "live_entries"];

/// One ratchet row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// `shipped.<triple>`, `lock`, or `patch`.
    pub scope: String,
    pub metric: String,
    /// The recorded high-water mark.
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

/// Read `<root>/tools/forge-budget.tsv` ([`run`]).
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

/// Parse a ratchet body.
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
                "comments are not supported: the file is pure TSV, read by one `awk -F'\\t'` \
                 idiom. Delete the line; the prose belongs in aterm_forge::budget's module docs",
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
                    "the 4th column is empty. It holds an accepted --allow-regress reason; \
                     delete the trailing tab if there is no regression to explain",
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
         shipped.aarch64-apple-darwin\\tthird_party_packages\\t28"
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

/// Is a `shipped.<triple>` metric armed (seeded, and owed a row)?
pub(crate) fn armed(metric: &str) -> bool {
    metric != "packages"
}

/// The armed rows, in file order.
fn seed_shape(live: &Live) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for scope in live.scopes.iter().filter(|s| s.starts_with("shipped.")) {
        for m in SHIPPED_METRICS.iter().filter(|m| armed(m)) {
            out.push((scope.clone(), (*m).to_string()));
        }
    }
    out.push(("lock".to_string(), "third_party_packages".to_string()));
    out.push(("patch".to_string(), "entries".to_string()));
    out.push(("patch".to_string(), "live_entries".to_string()));
    out
}

/// Extra per-scope facts that make a red row actionable.
#[derive(Clone, Debug, Default)]
struct ScopeDetail {
    duplicates: Vec<String>,
    build_scripts: Vec<String>,
    proc_macros: Vec<String>,
    biggest: Vec<(String, u64)>,
}

#[derive(Clone, Debug, Default)]
struct Live {
    values: BTreeMap<(String, String), u64>,
    /// Scopes in measurement order, for stable seeding.
    scopes: Vec<String>,
    /// Scope → why it could not be measured.
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

/// The ratchet scope one cell's rows live under; a scope names exactly one cell
/// ([`validate_metric`]).
pub(crate) fn scope_of(cell: &Cell, cells: &[Cell]) -> String {
    if cells.iter().filter(|c| c.triple == cell.triple).count() > 1 {
        format!("shipped.{}.{}", cell.triple, cell.name)
    } else {
        format!("shipped.{}", cell.triple)
    }
}

/// One cell's [`SHIPPED_METRICS`], in that order, as the ratchet reads them.
pub(crate) fn cell_metrics(s: &CellSurvey) -> [(&'static str, u64); 5] {
    [
        ("packages", u(s.graph.nodes.len())),
        ("third_party_packages", u(s.third_party().count())),
        ("build_scripts", u(s.build_scripts())),
        ("proc_macros", u(s.proc_macros())),
        ("duplicate_names", u(s.duplicate_names().len())),
    ]
}

fn measure(cargo: &crate::resolve::Cargo, root: &Path) -> Result<Live, String> {
    let mut live = Live::default();

    let cells = crate::resolve::default_cells();
    for cell in &cells {
        let scope = scope_of(cell, &cells);
        live.scopes.push(scope.clone());
        let mut resolver_log = String::new();
        let surveyed = crate::loc::survey_cell_logged(cargo, root, cell, &mut resolver_log);
        live.notes.extend(
            resolver_log
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(|l| format!("{}: {}", cell.name, l.trim())),
        );
        match surveyed {
            Ok(s) => {
                for (metric, value) in cell_metrics(&s) {
                    live.set(&scope, metric, value);
                }
                live.detail.insert(scope.clone(), detail(&s));
            }
            Err(e) => {
                live.unavailable.insert(scope.clone(), e);
            }
        }
    }

    let lock = crate::policy::lock_entries(root)?;
    let patches = crate::policy::patch_entries(root)?;
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
        let (label, whose) = if p.kind == aterm_census::scan_set::PatchTargetKind::Absorbed {
            (
                "ABSORBED PATCH",
                "It is upstream source aterm owns as a workspace member; `targo --unverified \
                 forge attest` also holds its NOTICE line, license text and §4(b) notices",
            )
        } else {
            ("FIRST-PARTY PATCH", "It is aterm's own crate")
        };
        live.notes.push(format!(
            "{label} (not counted in the `patch` ratchet, which bounds third-party \
             fork surface): {} → {} at {}, {}. {whose}; `targo --unverified forge attest` \
             [OB-1]/[OB-2] and `targo --unverified forge check` [OB-12] are what hold it to being present \
             and live.",
            p.name,
            p.path,
            p.manifest_version,
            if p.is_live() { "LIVE" } else { "NOT LIVE" }
        ));
    }
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

fn detail(s: &CellSurvey) -> ScopeDetail {
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
    }
}

/// What one row's comparison came to.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Verdict {
    /// Live is below the ceiling by `slack`.
    Under(u64),
    At,
    /// Live is above the ceiling by `over`.
    Over(u64),
    /// The scope could not be measured.
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
    if r.trim().is_empty() {
        return Err(format!(
            "the --allow-regress reason is empty; it is written into {BUDGET_PATH}, so say what \
             grew and why it had to"
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

const fn verdict(ceiling: u64, live: Option<u64>) -> Verdict {
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
    /// How many ceilings this run raised.
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
    for (scope, metric, value) in missing_armed(rows, live) {
        out.ok = false;
        out.problems.push(format!(
            "MISSING  {scope} {metric} — an armed fact with no row, so nothing holds it. \
             `--update` never adds a row; restore it (`git log -p -- {BUDGET_PATH}`), or arm \
             a new cell at its live value by adding this line:\n        {scope}\t{metric}\t{value}"
        ));
        out.table.push([
            scope,
            metric,
            "-".to_string(),
            value.to_string(),
            "RED (no row)".to_string(),
        ]);
    }
    out
}

/// Every armed fact ([`seed_shape`]) measured with no row in the file.
fn missing_armed(rows: &[Row], live: &Live) -> Vec<(String, String, u64)> {
    seed_shape(live)
        .into_iter()
        .filter(|(s, m)| !rows.iter().any(|r| &r.scope == s && &r.metric == m))
        .filter_map(|(s, m)| live.get(&s, &m).map(|v| (s, m, v)))
        .collect()
}

/// The red diagnostic for one row.
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
            "third_party_packages" => Some((
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
    let _ = write!(
        s,
        "      Either shrink it back, or record the regression:\n        targo --unverified forge budget \
         --update --allow-regress \"<what grew, and why it had to>\"",
    );
    s
}

/// Compare the live surface, as `cargo` resolves it, against `tools/forge-budget.tsv`.
pub fn run(
    cargo: &crate::resolve::Cargo,
    root: &Path,
    update: bool,
    allow_regress: Option<&str>,
) -> Result<Outcome, String> {
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
    let live = measure(cargo, root)?;

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
        log.push_str(
            "\n    UNRATCHETED (measured, not armed — `packages` counts aterm's own crates):\n",
        );
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
            "\n    VERDICT: RED — {} of {} rows over ceiling, unmeasured or missing.\n\n{}",
            plan.problems.len(),
            plan.rows.len(),
            crate::PRECISION_NOTE
        );
    }
    Ok(Outcome { ok, log })
}

/// The report when no ratchet file exists.
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

/// Every measured fact that is not armed and has no row in the file ([`missing_armed`]).
fn unratcheted(rows: &[Row], live: &Live) -> Vec<(String, String, u64)> {
    let armed = seed_shape(live);
    live.values
        .iter()
        .filter(|((s, m), _)| !rows.iter().any(|r| &r.scope == s && &r.metric == m))
        .filter(|(key, _)| !armed.contains(key))
        .map(|((s, m), v)| (s.clone(), m.clone(), *v))
        .collect()
}

/// A row naming a scope or metric forge does not measure is a broken ledger.
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

    const REASON: &str = "wgpu 29 pulled naga's new backend in for the WebGPU path";

    #[test]
    fn a_row_round_trips_through_render() {
        let text = format!(
            "shipped.aarch64-apple-darwin\tthird_party_packages\t28\npatch\tentries\t7\t{REASON}\n"
        );
        let rows = parse(&text).unwrap();
        assert_eq!(
            rows[0],
            row("shipped.aarch64-apple-darwin", "third_party_packages", 28)
        );
        assert_eq!(rows[1].regress_reason.as_deref(), Some(REASON));
        assert_eq!(render(&rows), text);
    }

    #[test]
    fn a_malformed_line_is_refused_by_line_number() {
        for (name, text, line) in [
            (
                "two columns",
                "lock\tthird_party_packages\t556\npatch\tentries\n",
                2,
            ),
            ("a word for a ceiling", "patch\tentries\tsix\n", 1),
            ("spaces for tabs", "patch entries 6\n", 1),
            ("a comment", "patch\tentries\t6\n# and here is why\n", 2),
            (
                "a duplicate row",
                "patch\tentries\t6\npatch\tentries\t7\n",
                2,
            ),
            ("an empty reason", "patch\tentries\t6\t\n", 1),
        ] {
            let e = parse(text).expect_err(name);
            assert!(
                e.starts_with(&format!("{BUDGET_PATH}:{line}:")),
                "{name}: {e}"
            );
        }
    }

    #[test]
    fn an_unknown_metric_or_scope_is_refused() {
        let live = live_with("lock", "packages", 624);
        assert!(validate_metric(&row("lock", "lines_of_code", 1), &live).is_err());
        assert!(validate_metric(&row("shipped.sparc-unknown-none", "packages", 1), &live).is_err());
    }

    #[test]
    fn update_lowers_a_ceiling_and_only_update_writes() {
        let live = live_with("patch", "entries", 4);
        let p = plan(&[row("patch", "entries", 6)], &live, true, None);
        assert!(p.ok && p.changed);
        assert_eq!(p.rows[0].ceiling, 4);
        let p = plan(&[row("patch", "entries", 6)], &live, false, None);
        assert!(p.ok && !p.changed);
        assert_eq!(p.rows[0].ceiling, 6);
    }

    #[test]
    fn a_raise_without_a_reason_is_red_and_writes_nothing() {
        let live = live_with("patch", "entries", 9);
        let p = plan(&[row("patch", "entries", 6)], &live, true, None);
        assert!(!p.ok);
        assert!(!p.changed, "a raise must never be written without a reason");
        assert_eq!(p.rows[0].ceiling, 6);
    }

    #[test]
    fn a_reason_is_accepted_and_recorded() {
        let reason = validate_reason(true, Some(REASON)).unwrap();
        assert_eq!(reason.as_deref(), Some(REASON));
        let live = live_with("patch", "entries", 9);
        let p = plan(
            &[row("patch", "entries", 6)],
            &live,
            true,
            reason.as_deref(),
        );
        assert!(p.ok && p.changed);
        assert_eq!(p.rows[0].ceiling, 9);
        assert_eq!(p.raised, 1);
        assert!(render(&p.rows).contains(REASON));
    }

    #[test]
    fn a_tab_in_the_reason_is_refused_because_the_file_is_tsv() {
        let r = format!("{REASON}\tand more");
        assert!(validate_reason(true, Some(&r)).is_err());
    }

    #[test]
    fn an_unmeasured_scope_is_never_a_pass() {
        let mut live = Live::default();
        live.unavailable.insert(
            "shipped.x86_64-pc-windows-msvc".to_string(),
            "no such target".to_string(),
        );
        let p = plan(
            &[row(
                "shipped.x86_64-pc-windows-msvc",
                "third_party_packages",
                10,
            )],
            &live,
            false,
            None,
        );
        assert!(!p.ok);
    }

    #[test]
    fn a_missing_armed_row_is_red_and_an_unarmed_one_advisory() {
        let scope = "shipped.aarch64-unknown-linux-gnu";
        let mut live = live_with(scope, "build_scripts", 25);
        live.set(scope, "packages", 400);
        live.set(scope, "proc_macros", 7);
        let rows = [row(scope, "build_scripts", 25)];
        for update in [false, true] {
            let p = plan(&rows, &live, update, None);
            assert!(!p.ok, "a deleted row must not leave the run green");
            assert_eq!(p.problems.len(), 1, "{:?}", p.problems);
            assert!(
                p.problems[0].contains(&format!("{scope}\tproc_macros\t7")),
                "{}",
                p.problems[0]
            );
            assert!(!p.changed, "--update never adds a row");
        }
        assert_eq!(
            unratcheted(&rows, &live),
            [(scope.to_string(), "packages".to_string(), 400)]
        );
    }
}
