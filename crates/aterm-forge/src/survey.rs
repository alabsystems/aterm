// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Third-party dependency inventory ranked by dominator cost
//! ([`crate::loc::survey_cell`], [`crate::dominator`]).

use crate::dominator::DomCost;
use crate::model::{Cell, CellSurvey, PkgId};
use crate::{Outcome, PRECISION_NOTE, dominator, loc, resolve};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

/// Line threshold below which a self-dominated third-party package is a replace-in-house candidate.
const ZERO_COST_LOC: u64 = 3_000;

const W_LOC: usize = 9;
const W_PKGS: usize = 4;
const W_NAME: usize = 33;
const W_ALSO: usize = 43;
/// Every emitted line stays inside this.
const W_LINE: usize = 96;

/// Emit the survey.
pub fn run(root: &Path, cells: &[String], top: usize) -> Result<Outcome, String> {
    let all = resolve::default_cells();
    let chosen = resolve::select(&all, cells)?;
    let (surveys, skipped) = survey_cells(root, &chosen).map_err(|why| {
        format!(
            "not one requested cell resolved:\n{why}    Fix: run `targo fetch --locked` once \
             in {} so the offline resolver has an index, then re-run `targo --unverified forge survey`.",
            root.display()
        )
    })?;

    let mut log = String::new();
    preamble(&mut log, root, &surveys);
    for s in &surveys {
        cell_section(&mut log, s, top);
    }
    cross_cell(&mut log, &surveys);

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
             `targo --unverified forge survey --cell <name>` so nothing claims to have been measured."
        );
        let _ = writeln!(log, "{PRECISION_NOTE}");
    }

    Ok(Outcome {
        ok: skipped.is_empty(),
        log,
    })
}

/// A cell that did not resolve.
type Skipped = (String, String);

/// Survey each chosen cell ([`run`], [`crate::blame::run`]).
pub(crate) fn survey_cells(
    root: &Path,
    chosen: &[Cell],
) -> Result<(Vec<CellSurvey>, Vec<Skipped>), String> {
    let mut surveys: Vec<CellSurvey> = Vec::new();
    let mut skipped: Vec<Skipped> = Vec::new();
    for cell in chosen {
        match loc::survey_cell(&crate::resolve::Cargo::from_env(), root, cell) {
            Ok(s) => surveys.push(s),
            Err(why) => skipped.push((cell.name.clone(), why)),
        }
    }
    if surveys.is_empty() {
        let mut why = String::new();
        for (c, e) in &skipped {
            let _ = writeln!(why, "      {c}: {e}");
        }
        return Err(why);
    }
    Ok((surveys, skipped))
}

fn preamble(out: &mut String, root: &Path, surveys: &[CellSurvey]) {
    let names: Vec<&str> = surveys.iter().map(|s| s.cell.name.as_str()).collect();
    let _ = writeln!(out, "{}", "=".repeat(W_LINE));
    let _ = writeln!(out, "FORGE SURVEY — aterm's third-party surface");
    let _ = writeln!(out, "{}", "=".repeat(W_LINE));
    let root_text = root.display().to_string();
    let root_line = if root_text.chars().count() > 88 {
        let tail: String = root_text
            .chars()
            .skip(root_text.chars().count() - 85)
            .collect();
        format!("...{tail}")
    } else {
        root_text
    };
    let _ = writeln!(out, "  root    {root_line}");
    let _ = writeln!(out, "  cells   {}", names.join(", "));
    let _ = writeln!(
        out,
        "  method  RESOLUTION ONLY — `targo tree -e normal --locked --offline`. No \
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
    let unsafe_total: u64 = s
        .third_party()
        .filter_map(|p| s.facts.get(p))
        .map(|f| f.unsafe_tokens)
        .sum();
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

    let mut covered: BTreeSet<&PkgId> = BTreeSet::new();
    for (id, cost) in ranked {
        for other in &cost.also {
            if other != id {
                covered.insert(other);
            }
        }
    }

    let shown = if top == 0 {
        ranked.len()
    } else {
        top.min(ranked.len())
    };
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

/// Checks that non-nested dominator sets partition all third-party nodes ([`crate::dominator`], [`crate::loc`]).
fn partition_check(
    out: &mut String,
    s: &CellSurvey,
    ranked: &[(PkgId, DomCost)],
    covered: &BTreeSet<&PkgId>,
) {
    let roots: Vec<&(PkgId, DomCost)> = ranked
        .iter()
        .filter(|(id, _)| !covered.contains(id))
        .collect();
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
             explained.\n    Fix: re-run `targo --unverified forge survey --cell {}` after `targo fetch \
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
    let _ = writeln!(
        out,
        "\n  DUPLICATE VERSIONS — one name resolved at two or more versions."
    );
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
    let _ = writeln!(
        out,
        "\n    {:<24}  {:<40}  {:>11}",
        "NAME", "VERSIONS", "DEDUP LOC"
    );
    let _ = writeln!(
        out,
        "    {}  {}  {}",
        "-".repeat(24),
        "-".repeat(40),
        "-".repeat(11)
    );
    let mut rows: Vec<(String, String, u64)> = Vec::new();
    let mut prize_total = 0u64;
    for (name, versions) in dups {
        let mut locs: Vec<u64> = versions
            .iter()
            .map(|v| {
                s.facts
                    .get(&PkgId::new(name.clone(), v.clone()))
                    .map_or(0, |f| f.loc)
            })
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
        let right = leaves
            .get(i + half)
            .map(|(id, l)| entry(id, *l))
            .unwrap_or_default();
        let line = format!("    {left:<40}    {right}");
        let _ = writeln!(out, "{}", line.trim_end());
    }

    let mut by_license: BTreeMap<&str, usize> = BTreeMap::new();
    for (id, _) in &leaves {
        let lic = s.facts.get(*id).map_or("", |f| f.license.as_str());
        *by_license
            .entry(if lic.is_empty() { "(unstated)" } else { lic })
            .or_insert(0) += 1;
    }
    let mut census: Vec<(&str, usize)> = by_license.into_iter().collect();
    census.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let text = census
        .iter()
        .map(|(lic, n)| format!("{lic} x{n}"))
        .collect::<Vec<_>>()
        .join(";  ");
    wrap(
        out,
        "    ",
        &format!("licences in this tier: {text}"),
        W_LINE,
    );
}

fn entry(id: &PkgId, loc_lines: u64) -> String {
    fit(
        &format!("{:>7}  {} {}", commas(loc_lines), id.name, id.version),
        40,
    )
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
        let unsafe_total: u64 = s
            .third_party()
            .filter_map(|p| s.facts.get(p))
            .map(|f| f.unsafe_tokens)
            .sum();
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
            "\n  One cell surveyed, so there is no cross-cell union — run `targo --unverified forge \
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

/// Thousands separators.
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

/// Word-wraps prose with runtime-measured values ([`crate::blame`]).
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

/// Truncate to `width` CHARACTERS.
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

/// Bounds a filesystem path to `width` characters by dropping leading components, else [`fit`].
pub(crate) fn fit_path(path: &str, width: usize) -> String {
    if path.chars().count() <= width {
        return path.to_string();
    }
    let parts: Vec<&str> = path.split('/').collect();
    for start in 1..parts.len() {
        let tail = parts[start..].join("/");
        if tail.chars().count() + 2 <= width {
            return format!("…/{tail}");
        }
    }
    fit(path, width)
}

/// The `also` list.
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
        let tail = if rest > 1 {
            format!(", +{} more", rest - 1)
        } else {
            String::new()
        };
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

/// Packages reachable FROM `id` (its subtree).
fn reach_from(s: &CellSurvey, id: &PkgId) -> BTreeSet<PkgId> {
    let mut seen = BTreeSet::new();
    seen.insert(id.clone());
    let mut stack = vec![id.clone()];
    while let Some(u) = stack.pop() {
        let Some(children) = s.graph.edges.get(&u) else {
            continue;
        };
        for v in children {
            if seen.insert(v.clone()) {
                stack.push(v.clone());
            }
        }
    }
    seen
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_truncates_by_chars_and_never_splits_one() {
        assert_eq!(fit("abc", 5), "abc");
        assert_eq!(fit("abcdef", 4), "abc…");
        assert_eq!(fit("ünïcödé-crate", 5), "ünïc…");
        assert_eq!(fit("abc", 0), "");
    }
}
