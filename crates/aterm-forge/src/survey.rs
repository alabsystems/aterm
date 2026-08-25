// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `cargo forge survey` — the inventory report, and the answer to "what are all
//! the third-party dependencies of aterm?".
//!
//! # Every number here is MEASURED, never transcribed
//!
//! The report is assembled from [`crate::loc::survey_cell`] (graph + per-package
//! facts) and [`crate::dominator`] (true cost) at run time. Nothing in the
//! emitted text is a hand-typed count: a number typed into prose rots the moment
//! the lock file moves, and this program exists precisely to catch that motion.
//!
//! # Why the table ranks by DOMINATOR cost
//!
//! `dom(C) = reach(root) \ reach(root, block C)` — the packages that disappear
//! if, and only if, `C` disappears. A SUBTREE size answers a different and
//! misleading question: it double-counts every dependency some other edge also
//! reaches. Measured example, recorded in [`crate::model`]: `softbuffer`'s
//! subtree is 39–45 packages resolved in isolation and **8** in the real Linux
//! graph, because `winit` already pulls `wayland-client` and `x11rb`. The table
//! also proves this live — it prints the top row's subtree beside its dominator.
//!
//! # The partition invariant
//!
//! Dominator sets nest (`dom(naga) ⊂ dom(wgpu)`), so the cost column must never
//! be summed blindly. The sets that do NOT nest inside another third-party
//! package's set are pairwise DISJOINT (dominators of a node form a chain), and
//! they cover every third-party node — so their LOC sums to the third-party
//! total exactly. The report computes that sum and prints the check, which is a
//! standing cross-check of [`crate::dominator`] against [`crate::loc`].

use crate::dominator::DomCost;
use crate::model::{CellSurvey, PkgId};
use crate::{Outcome, PRECISION_NOTE, dominator, loc, resolve};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

/// A third-party package whose dominator is exactly itself and whose source is
/// under this many lines is a REPLACE-IN-HOUSE candidate: deleting it cascades
/// to nothing, so it can be carved alone with no graph surgery and no version
/// negotiation with any other dependant.
const ZERO_COST_LOC: u64 = 3_000;

const W_LOC: usize = 9;
const W_PKGS: usize = 4;
const W_NAME: usize = 33;
const W_ALSO: usize = 43;
/// Every emitted line stays inside this, so an 100-column terminal never wraps
/// a table row into an unreadable second line.
const W_LINE: usize = 96;

/// Emit the survey. `cells` empty means every cell; `top` 0 means every row.
///
/// A cell that cannot be resolved is NAMED and the outcome is not `ok` — a
/// survey that silently measured three of four cells would understate the
/// surface, which is the exact failure mode this tool exists to prevent.
pub fn run(
    root: &Path,
    cells: &[String],
    top: usize,
    json: Option<&Path>,
) -> Result<Outcome, String> {
    let all = resolve::default_cells();
    let chosen = resolve::select(&all, cells)?;

    let mut surveys: Vec<CellSurvey> = Vec::new();
    let mut skipped: Vec<(String, String)> = Vec::new();
    for cell in &chosen {
        match loc::survey_cell(root, cell) {
            Ok(s) => surveys.push(s),
            Err(why) => skipped.push((cell.name.clone(), why)),
        }
    }

    if surveys.is_empty() {
        let mut why = String::new();
        for (c, e) in &skipped {
            let _ = writeln!(why, "      {c}: {e}");
        }
        return Err(format!(
            "not one requested cell resolved:\n{why}    Fix: run `cargo fetch --locked` once \
             in {} so the offline resolver has an index, then re-run `cargo forge survey`.",
            root.display()
        ));
    }

    let mut log = String::new();
    preamble(&mut log, root, &surveys);
    for s in &surveys {
        cell_section(&mut log, s, top);
    }
    cross_cell(&mut log, &surveys);

    if let Some(path) = json {
        let body = json_report(root, &surveys, &skipped);
        std::fs::write(path, &body).map_err(|e| {
            format!(
                "cannot write {}: {e} — Fix: pass `--json` a path in a directory that exists \
                 and is writable.",
                path.display()
            )
        })?;
        let _ = writeln!(log, "\n  JSON written: {} ({} bytes)", path.display(), body.len());
    }

    if !skipped.is_empty() {
        let _ = writeln!(log, "\n{}", "!".repeat(W_LINE));
        let _ = writeln!(
            log,
            "CELLS SKIPPED — this survey UNDERSTATES the surface by whatever they hold:"
        );
        for (c, e) in &skipped {
            let _ = writeln!(log, "      {c}: {e}");
        }
        let _ = writeln!(
            log,
            "    Fix: re-run with the cell reachable, or restrict the report honestly with\n    \
             `cargo forge survey --cell <name>` so nothing claims to have been measured."
        );
        let _ = writeln!(log, "{PRECISION_NOTE}");
    }

    Ok(Outcome { ok: skipped.is_empty(), log })
}

// ---------------------------------------------------------------- report body

fn preamble(out: &mut String, root: &Path, surveys: &[CellSurvey]) {
    let names: Vec<&str> = surveys.iter().map(|s| s.cell.name.as_str()).collect();
    let _ = writeln!(out, "{}", "=".repeat(W_LINE));
    let _ = writeln!(out, "FORGE SURVEY — aterm's third-party surface");
    let _ = writeln!(out, "{}", "=".repeat(W_LINE));
    let _ = writeln!(out, "  root    {}", root.display());
    let _ = writeln!(out, "  cells   {}", names.join(", "));
    let _ = writeln!(
        out,
        "  method  RESOLUTION ONLY — `cargo tree -e normal --locked --offline`. No \
         compiler,\n          no network. LOC is physical lines over every *.rs under a \
         package root\n          (rs-physical-all-files-v1), so it measures the source \
         aterm would OWN."
    );
}

fn cell_section(out: &mut String, s: &CellSurvey, top: usize) {
    let total = s.graph.nodes.len();
    let third = s.third_party().count();
    let workspace = total.saturating_sub(third);
    let loc_total = s.third_party_loc();
    let unsafe_total: u64 =
        s.third_party().filter_map(|p| s.facts.get(p)).map(|f| f.unsafe_tokens).sum();
    let dups = s.duplicate_names();

    let _ = writeln!(out, "\n{}", "=".repeat(W_LINE));
    let _ = writeln!(
        out,
        "CELL {}  —  {}  —  root package `{}`",
        s.cell.name, s.cell.triple, s.cell.package
    );
    let _ = writeln!(out, "{}", "=".repeat(W_LINE));
    let _ = writeln!(
        out,
        "  packages      {} resolved   {} workspace   {} THIRD-PARTY",
        commas(total as u64),
        commas(workspace as u64),
        commas(third as u64)
    );
    let _ = writeln!(
        out,
        "  third-party   {} LOC   {} unsafe tokens   {} build scripts   {} proc macros",
        commas(loc_total),
        commas(unsafe_total),
        commas(s.build_scripts() as u64),
        commas(s.proc_macros() as u64)
    );
    let _ = writeln!(
        out,
        "  duplicates    {} names resolved at two or more versions",
        commas(dups.len() as u64)
    );

    let ranked = dominator::ranked(s);
    ranked_table(out, s, &ranked, top);
    duplicate_section(out, s, &dups);
    zero_cost_section(out, s, &ranked);
}

/// The ranked table, plus the two things that make it honest: the live
/// subtree-vs-dominator comparison, and the partition check.
fn ranked_table(out: &mut String, s: &CellSurvey, ranked: &[(PkgId, DomCost)], top: usize) {
    let _ = writeln!(
        out,
        "\n  RANKED BY DOMINATOR COST  —  dom(C) = reach(root) \\ reach(root, block C)"
    );
    let _ = writeln!(
        out,
        "  This is what deleting C would ACTUALLY remove, not C's subtree. A subtree\n  \
         double-counts every dependency another edge also reaches (recorded measurement:\n  \
         softbuffer's subtree is 39-45 packages resolved alone and 8 in the real linux graph,\n  \
         because winit already pulls wayland-client and x11rb)."
    );
    if let Some((id, cost)) = ranked.first() {
        let subtree = reach_from(s, id).len();
        let _ = writeln!(
            out,
            "  Live proof in this cell: {} {}'s subtree is {} packages; its dominator cost is {}.",
            id.name,
            id.version,
            commas(subtree as u64),
            commas(cost.pkgs as u64)
        );
    }

    // A row is NESTED when some other row's dominator set already contains it:
    // deleting the outer row deletes this one too, so the two costs must never
    // be added together.
    let mut covered: BTreeSet<&PkgId> = BTreeSet::new();
    for (id, cost) in ranked {
        for other in &cost.also {
            if other != id {
                covered.insert(other);
            }
        }
    }

    let shown = if top == 0 { ranked.len() } else { top.min(ranked.len()) };
    let _ = writeln!(
        out,
        "  `.` marks a NESTED row: it is already inside a higher row's cost. The column\n  \
         therefore does not sum — see the partition check below it."
    );
    let _ = writeln!(
        out,
        "\n    {:>lw$}  {:>pw$}  {:<nw$}  ALSO REMOVES (largest first)",
        "EXCL LOC",
        "PKGS",
        "PACKAGE",
        lw = W_LOC,
        pw = W_PKGS,
        nw = W_NAME
    );
    let _ = writeln!(
        out,
        "    {}  {}  {}  {}",
        "-".repeat(W_LOC),
        "-".repeat(W_PKGS),
        "-".repeat(W_NAME),
        "-".repeat(W_ALSO)
    );
    for (id, cost) in ranked.iter().take(shown) {
        let mark = if covered.contains(id) { '.' } else { ' ' };
        let line = format!(
            "  {mark} {:>lw$}  {:>pw$}  {:<nw$}  {}",
            commas(cost.loc),
            commas(cost.pkgs as u64),
            fit(&format!("{} {}", id.name, id.version), W_NAME),
            also_column(s, &cost.also, id, W_ALSO),
            lw = W_LOC,
            pw = W_PKGS,
            nw = W_NAME
        );
        let _ = writeln!(out, "{}", line.trim_end());
    }
    if shown < ranked.len() {
        let _ = writeln!(
            out,
            "    ... {} more rows — `--top {}` for all of them, `--top 0` for every full table",
            commas((ranked.len() - shown) as u64),
            ranked.len()
        );
    }

    partition_check(out, s, ranked, &covered);
}

/// Non-nested dominator sets are pairwise disjoint and cover every third-party
/// node, so their LOC must sum to the third-party total. Printing the identity
/// makes any disagreement between [`crate::dominator`] and [`crate::loc`] a
/// visible line rather than a silently wrong ranking.
fn partition_check(
    out: &mut String,
    s: &CellSurvey,
    ranked: &[(PkgId, DomCost)],
    covered: &BTreeSet<&PkgId>,
) {
    let roots: Vec<&(PkgId, DomCost)> =
        ranked.iter().filter(|(id, _)| !covered.contains(id)).collect();
    let sum: u64 = roots.iter().map(|(_, c)| c.loc).sum();
    let pkgs: usize = roots.iter().map(|(_, c)| c.pkgs).sum();
    let total = s.third_party_loc();
    let third = s.third_party().count();
    if sum == total && pkgs == third {
        let _ = writeln!(
            out,
            "    PARTITION CHECK OK: the {} non-nested rows are disjoint and cover the \
             surface\n    \
             ({} packages, {} LOC = the third-party total). Carving them is additive.",
            commas(roots.len() as u64),
            commas(pkgs as u64),
            commas(sum)
        );
    } else {
        let _ = writeln!(
            out,
            "    PARTITION CHECK DISAGREES: {} non-nested rows cover {} packages / {} LOC, \
             but the\n    cell holds {} / {}. The ranking is indicative only until that is \
             explained.\n    Fix: re-run `cargo forge survey --cell {}` after `cargo fetch \
             --locked`; if it persists, the dominator sets and the fact table disagree and \
             dominator.rs is the place to look.",
            commas(roots.len() as u64),
            commas(pkgs as u64),
            commas(sum),
            commas(third as u64),
            commas(total),
            s.cell.name
        );
    }
}

fn duplicate_section(out: &mut String, s: &CellSurvey, dups: &BTreeMap<String, Vec<String>>) {
    let _ = writeln!(out, "\n  DUPLICATE VERSIONS — one name resolved at two or more versions.");
    if dups.is_empty() {
        let _ = writeln!(out, "    none in this cell.");
        return;
    }
    let _ = writeln!(
        out,
        "  Collapsing a duplicate is a DEDUP, not a removal: the prize is the LOC of every\n  \
         copy but the largest, and it is won by moving a dependant's requirement, not by\n  \
         deleting code."
    );
    let _ = writeln!(out, "\n    {:<24}  {:<40}  {:>11}", "NAME", "VERSIONS", "DEDUP LOC");
    let _ = writeln!(out, "    {}  {}  {}", "-".repeat(24), "-".repeat(40), "-".repeat(11));
    let mut rows: Vec<(String, String, u64)> = Vec::new();
    let mut prize_total = 0u64;
    for (name, versions) in dups {
        let mut locs: Vec<u64> = versions
            .iter()
            .map(|v| s.facts.get(&PkgId::new(name.clone(), v.clone())).map_or(0, |f| f.loc))
            .collect();
        locs.sort_unstable();
        let prize: u64 = locs.iter().rev().skip(1).sum();
        prize_total += prize;
        rows.push((name.clone(), versions.join(", "), prize));
    }
    rows.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
    for (name, versions, prize) in &rows {
        let line = format!(
            "    {:<24}  {:<40}  {:>11}",
            fit(name, 24),
            fit(versions, 40),
            commas(*prize)
        );
        let _ = writeln!(out, "{}", line.trim_end());
    }
    let _ = writeln!(
        out,
        "    TOTAL dedup prize in this cell: {} LOC across {} names.",
        commas(prize_total),
        commas(rows.len() as u64)
    );
}

fn zero_cost_section(out: &mut String, s: &CellSurvey, ranked: &[(PkgId, DomCost)]) {
    let mut leaves: Vec<(&PkgId, u64)> = ranked
        .iter()
        .filter(|(_, c)| c.pkgs == 1 && c.loc < ZERO_COST_LOC)
        .map(|(id, c)| (id, c.loc))
        .collect();
    leaves.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));

    let sum: u64 = leaves.iter().map(|(_, l)| *l).sum();
    let _ = writeln!(
        out,
        "\n  ZERO-COST LEAVES — dominator is exactly 1 package and under {} LOC.",
        commas(ZERO_COST_LOC)
    );
    if leaves.is_empty() {
        let _ = writeln!(out, "    none in this cell.");
        return;
    }
    let _ = writeln!(
        out,
        "  Each removes ONLY itself, so each is carveable alone — no cascade, no version\n  \
         negotiation, no graph surgery. {} of them, {} LOC in total.",
        commas(leaves.len() as u64),
        commas(sum)
    );
    let half = leaves.len().div_ceil(2);
    for i in 0..half {
        let left = entry(leaves[i].0, leaves[i].1);
        let right = leaves.get(i + half).map(|(id, l)| entry(id, *l)).unwrap_or_default();
        let line = format!("    {left:<40}    {right}");
        let _ = writeln!(out, "{}", line.trim_end());
    }

    // The SPDX expression decides whether replacing one in-house is even
    // allowed, so it is censused in full rather than truncated into a column.
    let mut by_license: BTreeMap<&str, usize> = BTreeMap::new();
    for (id, _) in &leaves {
        let lic = s.facts.get(*id).map_or("", |f| f.license.as_str());
        *by_license.entry(if lic.is_empty() { "(unstated)" } else { lic }).or_insert(0) += 1;
    }
    let mut census: Vec<(&str, usize)> = by_license.into_iter().collect();
    census.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let text = census
        .iter()
        .map(|(lic, n)| format!("{lic} x{n}"))
        .collect::<Vec<_>>()
        .join(";  ");
    wrap(out, "    ", &format!("licences in this tier: {text}"), W_LINE);
}

fn entry(id: &PkgId, loc_lines: u64) -> String {
    fit(&format!("{:>7}  {} {}", commas(loc_lines), id.name, id.version), 40)
}

fn cross_cell(out: &mut String, surveys: &[CellSurvey]) {
    let _ = writeln!(out, "\n{}", "=".repeat(W_LINE));
    let _ = writeln!(out, "CROSS-CELL SUMMARY");
    let _ = writeln!(out, "{}", "=".repeat(W_LINE));
    let _ = writeln!(
        out,
        "    {:<9}{:>6}{:>6}{:>13}{:>12}{:>9}{:>11}{:>7}{:>6}",
        "CELL", "TOTAL", "WS", "THIRD-PTY", "LOC", "UNSAFE", "BUILD.RS", "PROC", "DUP"
    );
    let _ = writeln!(out, "    {}", "-".repeat(79));
    for s in surveys {
        let total = s.graph.nodes.len();
        let third = s.third_party().count();
        let unsafe_total: u64 =
            s.third_party().filter_map(|p| s.facts.get(p)).map(|f| f.unsafe_tokens).sum();
        let _ = writeln!(
            out,
            "    {:<9}{:>6}{:>6}{:>13}{:>12}{:>9}{:>11}{:>7}{:>6}",
            fit(&s.cell.name, 9),
            commas(total as u64),
            commas(total.saturating_sub(third) as u64),
            commas(third as u64),
            commas(s.third_party_loc()),
            commas(unsafe_total),
            commas(s.build_scripts() as u64),
            commas(s.proc_macros() as u64),
            commas(s.duplicate_names().len() as u64)
        );
    }

    let mut seen_in: BTreeMap<&PkgId, usize> = BTreeMap::new();
    for s in surveys {
        for p in s.third_party() {
            *seen_in.entry(p).or_insert(0) += 1;
        }
    }
    let union = seen_in.len();
    let everywhere = seen_in.values().filter(|n| **n == surveys.len()).count();
    let only_one = seen_in.values().filter(|n| **n == 1).count();
    if surveys.len() < 2 {
        let _ = writeln!(
            out,
            "\n  One cell surveyed, so there is no cross-cell union — run `cargo forge \
             survey`\n  with no `--cell` for the union and the target-specific split."
        );
        return;
    }
    let _ = writeln!(
        out,
        "\n  UNION of third-party packages across the {} cells surveyed: {}",
        commas(surveys.len() as u64),
        commas(union as u64)
    );
    let _ = writeln!(
        out,
        "  In EVERY surveyed cell: {}   —   in exactly ONE cell: {} (target-specific surface).",
        commas(everywhere as u64),
        commas(only_one as u64)
    );
}

// ------------------------------------------------------------------ formatting

/// Thousands separators. Big numbers are the point of this report and an
/// unseparated seven-digit column is unreadable at a glance.
pub(crate) fn commas(n: u64) -> String {
    let digits = n.to_string();
    let len = digits.len();
    let mut out = String::with_capacity(len + len / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Word-wrap prose that interpolates measured values. Pre-wrapping in the
/// source cannot work: one long crate name or a seven-digit count would push
/// the line past the terminal, and a report nobody can read is a report nobody
/// reads. Shared with [`crate::blame`] so both verbs wrap identically.
pub(crate) fn wrap(out: &mut String, indent: &str, text: &str, width: usize) {
    for para in text.split('\n') {
        let mut line = String::from(indent);
        let mut empty = true;
        for word in para.split_whitespace() {
            if !empty && line.chars().count() + 1 + word.chars().count() > width {
                let _ = writeln!(out, "{}", line.trim_end());
                line = String::from(indent);
                empty = true;
            }
            if !empty {
                line.push(' ');
            }
            line.push_str(word);
            empty = false;
        }
        let _ = writeln!(out, "{}", line.trim_end());
    }
}

/// Truncate to `width` CHARACTERS (never bytes — a byte slice can split a
/// multi-byte name and panic), marking the cut with a single ellipsis.
pub(crate) fn fit(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut out: String = s.chars().take(width - 1).collect();
    out.push('…');
    out
}

/// The `also` list, largest package first, packed into `width` columns with a
/// trailing `+N more` so the count is never lost to truncation.
fn also_column(s: &CellSurvey, also: &[PkgId], target: &PkgId, width: usize) -> String {
    let mut items: Vec<(&PkgId, u64)> = also
        .iter()
        .filter(|p| *p != target)
        .map(|p| (p, s.facts.get(p).map_or(0, |f| f.loc)))
        .collect();
    if items.is_empty() {
        return "(leaf: removes only itself)".to_string();
    }
    items.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));

    let mut out = String::new();
    let mut shown = 0usize;
    for (i, (p, _)) in items.iter().enumerate() {
        let text = format!("{} {}", p.name, p.version);
        let sep = if i == 0 { "" } else { ", " };
        let rest = items.len() - i;
        let tail = if rest > 1 { format!(", +{} more", rest - 1) } else { String::new() };
        let width_if_taken =
            out.chars().count() + sep.chars().count() + text.chars().count() + tail.chars().count();
        if i > 0 && width_if_taken > width {
            break;
        }
        out.push_str(sep);
        out.push_str(&text);
        shown += 1;
    }
    if shown < items.len() {
        let _ = write!(out, ", +{} more", items.len() - shown);
    }
    fit(&out, width)
}

/// Packages reachable FROM `id` (its subtree) — reported only next to the
/// dominator cost, as the live demonstration that the two differ.
fn reach_from(s: &CellSurvey, id: &PkgId) -> BTreeSet<PkgId> {
    let mut seen = BTreeSet::new();
    seen.insert(id.clone());
    let mut stack = vec![id.clone()];
    while let Some(u) = stack.pop() {
        let Some(children) = s.graph.edges.get(&u) else { continue };
        for v in children {
            if seen.insert(v.clone()) {
                stack.push(v.clone());
            }
        }
    }
    seen
}

// ------------------------------------------------------------------------ JSON

/// Hand-rolled JSON. No `serde_json`: a program whose entire purpose is to
/// shrink the third-party surface does not get to widen it to print a report.
fn json_report(root: &Path, surveys: &[CellSurvey], skipped: &[(String, String)]) -> String {
    let mut o = String::with_capacity(1 << 16);
    o.push_str("{\n");
    o.push_str("  \"tool\": \"aterm-forge\",\n");
    o.push_str("  \"report\": \"survey\",\n");
    o.push_str("  \"root\": ");
    jstr(&mut o, &root.display().to_string());
    o.push_str(",\n");
    o.push_str("  \"loc_method\": \"rs-physical-all-files-v1\",\n");
    o.push_str("  \"cost_method\": ");
    jstr(&mut o, "dominator: dom(C) = reach(root) \\ reach(root, block C)");
    o.push_str(",\n");
    o.push_str("  \"cells\": [\n");
    for (i, s) in surveys.iter().enumerate() {
        json_cell(&mut o, s);
        o.push_str(if i + 1 == surveys.len() { "\n" } else { ",\n" });
    }
    o.push_str("  ],\n");

    let mut seen_in: BTreeMap<&PkgId, usize> = BTreeMap::new();
    for s in surveys {
        for p in s.third_party() {
            *seen_in.entry(p).or_insert(0) += 1;
        }
    }
    o.push_str("  \"union_third_party\": ");
    o.push_str(&seen_in.len().to_string());
    o.push_str(",\n  \"in_every_cell\": ");
    o.push_str(&seen_in.values().filter(|n| **n == surveys.len()).count().to_string());
    o.push_str(",\n  \"skipped\": [");
    for (i, (cell, why)) in skipped.iter().enumerate() {
        if i > 0 {
            o.push(',');
        }
        o.push_str("\n    {\"cell\": ");
        jstr(&mut o, cell);
        o.push_str(", \"reason\": ");
        jstr(&mut o, why);
        o.push('}');
    }
    if !skipped.is_empty() {
        o.push('\n');
        o.push_str("  ");
    }
    o.push_str("]\n}\n");
    o
}

fn json_cell(o: &mut String, s: &CellSurvey) {
    let ranked = dominator::ranked(s);
    let total = s.graph.nodes.len();
    let third = s.third_party().count();
    let unsafe_total: u64 =
        s.third_party().filter_map(|p| s.facts.get(p)).map(|f| f.unsafe_tokens).sum();

    o.push_str("    {\n      \"name\": ");
    jstr(o, &s.cell.name);
    o.push_str(",\n      \"triple\": ");
    jstr(o, &s.cell.triple);
    o.push_str(",\n      \"package\": ");
    jstr(o, &s.cell.package);
    o.push_str(",\n      \"totals\": {");
    jnum(o, "packages", total as u64);
    o.push_str(", ");
    jnum(o, "workspace", total.saturating_sub(third) as u64);
    o.push_str(", ");
    jnum(o, "third_party", third as u64);
    o.push_str(", ");
    jnum(o, "third_party_loc", s.third_party_loc());
    o.push_str(", ");
    jnum(o, "unsafe_tokens", unsafe_total);
    o.push_str(", ");
    jnum(o, "build_scripts", s.build_scripts() as u64);
    o.push_str(", ");
    jnum(o, "proc_macros", s.proc_macros() as u64);
    o.push_str(", ");
    jnum(o, "duplicate_names", s.duplicate_names().len() as u64);
    o.push_str("},\n      \"ranked\": [");
    for (i, (id, cost)) in ranked.iter().enumerate() {
        if i > 0 {
            o.push(',');
        }
        o.push_str("\n        {");
        jkv(o, "name", &id.name);
        o.push_str(", ");
        jkv(o, "version", &id.version);
        o.push_str(", ");
        jnum(o, "dom_pkgs", cost.pkgs as u64);
        o.push_str(", ");
        jnum(o, "dom_loc", cost.loc);
        o.push_str(", \"also\": [");
        for (j, p) in cost.also.iter().filter(|p| *p != id).enumerate() {
            if j > 0 {
                o.push_str(", ");
            }
            jstr(o, &p.spec());
        }
        o.push_str("]}");
    }
    o.push_str("\n      ],\n      \"duplicates\": [");
    for (i, (name, versions)) in s.duplicate_names().iter().enumerate() {
        if i > 0 {
            o.push(',');
        }
        let mut locs: Vec<u64> = versions
            .iter()
            .map(|v| s.facts.get(&PkgId::new(name.clone(), v.clone())).map_or(0, |f| f.loc))
            .collect();
        locs.sort_unstable();
        o.push_str("\n        {");
        jkv(o, "name", name);
        o.push_str(", \"versions\": [");
        for (j, v) in versions.iter().enumerate() {
            if j > 0 {
                o.push_str(", ");
            }
            jstr(o, v);
        }
        o.push_str("], ");
        jnum(o, "dedup_loc", locs.iter().rev().skip(1).sum::<u64>());
        o.push('}');
    }
    o.push_str("\n      ],\n      \"packages\": [");
    let mut wrote = false;
    for id in &s.graph.nodes {
        let Some(f) = s.facts.get(id) else { continue };
        if wrote {
            o.push(',');
        }
        wrote = true;
        o.push_str("\n        {");
        jkv(o, "name", &id.name);
        o.push_str(", ");
        jkv(o, "version", &id.version);
        o.push_str(", ");
        jnum(o, "loc", f.loc);
        o.push_str(", ");
        jnum(o, "unsafe_tokens", f.unsafe_tokens);
        o.push_str(", \"build_rs\": ");
        o.push_str(if f.has_build_rs { "true" } else { "false" });
        o.push_str(", \"proc_macro\": ");
        o.push_str(if f.is_proc_macro { "true" } else { "false" });
        o.push_str(", \"third_party\": ");
        o.push_str(if f.is_third_party { "true" } else { "false" });
        o.push_str(", ");
        jkv(o, "license", &f.license);
        o.push('}');
    }
    o.push_str("\n      ]\n    }");
}

fn jkv(o: &mut String, key: &str, val: &str) {
    o.push('"');
    o.push_str(key);
    o.push_str("\": ");
    jstr(o, val);
}

fn jnum(o: &mut String, key: &str, n: u64) {
    o.push('"');
    o.push_str(key);
    o.push_str("\": ");
    o.push_str(&n.to_string());
}

/// A JSON string literal, quotes included. Escapes exactly what RFC 8259
/// requires: the two structural characters and every C0 control.
fn jstr(o: &mut String, s: &str) {
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            '\u{08}' => o.push_str("\\b"),
            '\u{0c}' => o.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(o, "\\u{:04x}", c as u32);
            }
            c => o.push(c),
        }
    }
    o.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::measured;

    fn repo_root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("crates/aterm-forge sits two levels under the workspace root")
            .to_path_buf()
    }

    fn cell(name: &str) -> crate::model::Cell {
        resolve::default_cells()
            .into_iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("no cell named {name}"))
    }

    #[test]
    fn commas_groups_from_the_right() {
        assert_eq!(commas(0), "0");
        assert_eq!(commas(999), "999");
        assert_eq!(commas(1_000), "1,000");
        assert_eq!(commas(1_234_567), "1,234,567");
        assert_eq!(commas(u64::MAX), "18,446,744,073,709,551,615");
    }

    #[test]
    fn fit_truncates_by_chars_and_never_splits_one() {
        assert_eq!(fit("abc", 5), "abc");
        assert_eq!(fit("abcdef", 4), "abc…");
        // A multi-byte name must not panic or produce invalid UTF-8.
        assert_eq!(fit("ünïcödé-crate", 5), "ünïc…");
        assert_eq!(fit("abc", 0), "");
    }

    #[test]
    fn json_escapes_quotes_backslashes_and_controls() {
        let mut o = String::new();
        jstr(&mut o, r#"he said "hi" \ then"#);
        assert_eq!(o, r#""he said \"hi\" \\ then""#);

        let mut o = String::new();
        jstr(&mut o, "a\nb\tc\rd\u{08}e\u{0c}f\u{1}g");
        assert_eq!(o, r#""a\nb\tc\rd\be\ff\u0001g""#);

        // A Windows-shaped path is the realistic backslash case.
        let mut o = String::new();
        jstr(&mut o, r"C:\Users\a\.cargo");
        assert_eq!(o, r#""C:\\Users\\a\\.cargo""#);
    }

    /// A hand-rolled emitter needs a structural reader to answer to, or a
    /// stray comma ships unnoticed. This is a minimal but complete RFC 8259
    /// value scanner: it accepts exactly well-formed JSON.
    fn json_wellformed(s: &str) -> Result<(), String> {
        let b: Vec<char> = s.chars().collect();
        let mut i = 0usize;
        fn ws(b: &[char], i: &mut usize) {
            while *i < b.len() && matches!(b[*i], ' ' | '\n' | '\t' | '\r') {
                *i += 1;
            }
        }
        fn value(b: &[char], i: &mut usize) -> Result<(), String> {
            ws(b, i);
            let Some(&c) = b.get(*i) else { return Err(format!("value expected at {i}")) };
            match c {
                '{' | '[' => {
                    let close = if c == '{' { '}' } else { ']' };
                    *i += 1;
                    ws(b, i);
                    if b.get(*i) == Some(&close) {
                        *i += 1;
                        return Ok(());
                    }
                    loop {
                        ws(b, i);
                        if close == '}' {
                            string(b, i)?;
                            ws(b, i);
                            if b.get(*i) != Some(&':') {
                                return Err(format!("`:` expected at {i}"));
                            }
                            *i += 1;
                        }
                        value(b, i)?;
                        ws(b, i);
                        match b.get(*i) {
                            Some(',') => *i += 1,
                            Some(&x) if x == close => {
                                *i += 1;
                                return Ok(());
                            }
                            other => {
                                return Err(format!("`,`/`{close}` wanted at {i}: {other:?}"));
                            }
                        }
                    }
                }
                '"' => string(b, i),
                't' | 'f' | 'n' => {
                    let word = if c == 't' { "true" } else if c == 'f' { "false" } else { "null" };
                    for w in word.chars() {
                        if b.get(*i) != Some(&w) {
                            return Err(format!("bad literal at {i}"));
                        }
                        *i += 1;
                    }
                    Ok(())
                }
                c if c == '-' || c.is_ascii_digit() => {
                    *i += 1;
                    while b.get(*i).is_some_and(|d| d.is_ascii_digit() || *d == '.') {
                        *i += 1;
                    }
                    Ok(())
                }
                other => Err(format!("unexpected `{other}` at {i}")),
            }
        }
        fn string(b: &[char], i: &mut usize) -> Result<(), String> {
            if b.get(*i) != Some(&'"') {
                return Err(format!("string expected at {i}"));
            }
            *i += 1;
            loop {
                match b.get(*i) {
                    None => return Err("unterminated string".into()),
                    Some('\\') => {
                        *i += 2;
                    }
                    Some('"') => {
                        *i += 1;
                        return Ok(());
                    }
                    Some(&c) if (c as u32) < 0x20 => {
                        return Err(format!("raw control char in string at {i}"));
                    }
                    Some(_) => *i += 1,
                }
            }
        }
        value(&b, &mut i)?;
        ws(&b, &mut i);
        if i != b.len() {
            return Err(format!("trailing bytes at {i}"));
        }
        Ok(())
    }

    #[test]
    fn the_json_validator_rejects_what_it_should() {
        assert!(json_wellformed(r#"{"a": [1, 2], "b": {"c": true}}"#).is_ok());
        assert!(json_wellformed(r#"{"a": [1, 2,]}"#).is_err());
        assert!(json_wellformed(r#"{"a" 1}"#).is_err());
        assert!(json_wellformed(r#"{"a": "b}"#).is_err());
    }

    /// THE ground-truth test: the report's own assembly path reproduces the
    /// baseline in `measured`, which was taken with an independent `cargo tree`
    /// plus source walk.
    #[test]
    fn mac_arm_reproduces_the_measured_third_party_surface() {
        let root = repo_root();
        let want = measured::MAC_ARM;
        let s = loc::survey_cell(&root, &cell("mac-arm")).expect("mac-arm resolves offline");
        assert_eq!(s.graph.nodes.len(), want.resolved, "total packages in the shipped graph");
        assert_eq!(s.third_party().count(), want.third_party, "third-party packages");
        assert_eq!(s.third_party_loc(), want.third_party_loc, "third-party physical LOC");
        assert_eq!(s.duplicate_names().len(), want.duplicate_names, "names at 2+ versions");
    }

    /// The invariant the report prints: non-nested dominator sets are disjoint
    /// and cover the surface, so their LOC sums to the third-party total.
    #[test]
    fn non_nested_dominator_sets_partition_the_surface() {
        let root = repo_root();
        let s = loc::survey_cell(&root, &cell("mac-arm")).expect("mac-arm resolves offline");
        let ranked = dominator::ranked(&s);
        let mut covered: BTreeSet<&PkgId> = BTreeSet::new();
        for (id, cost) in &ranked {
            for other in &cost.also {
                if other != id {
                    covered.insert(other);
                }
            }
        }
        let roots: Vec<&(PkgId, DomCost)> =
            ranked.iter().filter(|(id, _)| !covered.contains(id)).collect();
        assert_eq!(
            roots.iter().map(|(_, c)| c.loc).sum::<u64>(),
            s.third_party_loc(),
            "non-nested dominator LOC must sum to the third-party total"
        );
        assert_eq!(
            roots.iter().map(|(_, c)| c.pkgs).sum::<usize>(),
            s.third_party().count(),
            "non-nested dominator sets must cover every third-party package exactly once"
        );
    }

    #[test]
    fn the_report_names_the_measured_numbers_and_stays_inside_the_terminal() {
        let root = repo_root();
        let out = run(&root, &["mac-arm".to_string()], 12, None).expect("survey runs");
        assert!(out.ok, "a resolvable cell is not a failure:\n{}", out.log);
        let want = measured::MAC_ARM;
        let loc_text = commas(want.third_party_loc);
        assert!(out.log.contains(&loc_text), "third-party LOC is printed:\n{}", out.log);
        assert!(
            out.log.contains(&want.third_party.to_string()),
            "third-party count is printed"
        );
        assert!(out.log.contains("PARTITION CHECK OK"), "the check must hold:\n{}", out.log);
        assert!(out.log.contains("ZERO-COST LEAVES"));
        assert!(out.log.contains("DUPLICATE VERSIONS"));
        for line in out.log.lines() {
            assert!(
                line.chars().count() <= 100,
                "line of {} columns breaks a 100-column terminal: {line:?}",
                line.chars().count()
            );
        }
    }

    #[test]
    fn json_output_is_wellformed_and_carries_the_measured_totals() {
        let root = repo_root();
        let path = std::env::temp_dir()
            .join(format!("forge-survey-{}-{}.json", std::process::id(), line!()));
        let out = run(&root, &["mac-arm".to_string()], 5, Some(&path)).expect("survey runs");
        assert!(out.ok);
        let body = std::fs::read_to_string(&path).expect("the JSON file was written");
        let _ = std::fs::remove_file(&path);
        json_wellformed(&body).unwrap_or_else(|e| panic!("hand-rolled JSON is malformed: {e}"));
        let want = measured::MAC_ARM;
        assert!(
            body.contains(&format!("\"third_party\": {}", want.third_party)),
            "measured count in JSON"
        );
        assert!(
            body.contains(&format!("\"third_party_loc\": {}", want.third_party_loc)),
            "measured LOC in JSON"
        );
        // `--top` is a DISPLAY bound; the machine-readable form keeps every row.
        let rows = body.matches("\"dom_pkgs\"").count();
        assert_eq!(
            rows, want.third_party,
            "every third-party row is in the JSON regardless of --top"
        );
    }

    #[test]
    fn an_unknown_cell_is_refused_by_resolve_not_silently_ignored() {
        let root = repo_root();
        let Err(err) = run(&root, &["mars".to_string()], 5, None) else {
            panic!("an unknown cell name must be refused, not silently dropped")
        };
        assert!(!err.is_empty(), "a refusal must say something");
        assert!(
            err.contains("mars") || err.contains("mac-arm"),
            "the refusal must name the bad cell or the real ones: {err}"
        );
    }
}
