// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! mirror-rows: a mirror proof is credited only while the code and the clause are the ones it
//! proved, and only for the rows a note names.
//!
//! Clean proves some aterm items through a mirror harness: a copy of aterm code in Clean's
//! tree, proved against the SPECS.md clause. Nothing ties the copy to aterm, so after an edit
//! of the code or the clause a note "proved via mirror" goes on claiming a proof of code that
//! is gone. [`crate::mirror::REGISTRY`] records each proved row with the digests of the items
//! its harness copied and of its clause as proved; this rule recomputes both from the tree.
//!
//! Every registry row with a clause is named by exactly one note on that clause's notes
//! column: `Proved via mirror: Clean R<n> row(s) …` while its digests hold, or `Stale proof:
//! Clean R<n> row(s) …` once they moved (a stale row claims nothing, so nothing more about it
//! is checked but that it can still be read). A row with no clause has no note: it holds, or
//! is proved again, or is forgotten.
//!
//! Findings:
//! - a claimed row whose items or clause changed;
//! - a row the tree cannot read (its package, file, item or clause gone, or the clause id
//!   given two rows);
//! - a note naming a row the registry does not record, or records under another Clean entry,
//!   package or clause; a registry row no note names, or more than one names;
//! - a passage that reads as a mirror-proof claim in neither note form, or either form outside
//!   a clause's notes cell (in the statement, in prose).

use std::borrow::Cow;
use std::collections::BTreeMap;

use super::{Finding, Rule, Tree};
use crate::mirror::{self, CLAIM, NO_CLAUSE, REGISTRY, Row, STALE};

pub(super) const MIRROR_ROWS: Rule = Rule {
    id: "mirror-rows",
    why: "a mirror proof is a proof of a copy: once aterm's code or its SPECS.md clause \
          changes, the copy is not the code, so the registry records each proved row's digests \
          and the row's note says \"Stale proof\" until Clean proves it again",
    scope: &[],
    check: mirror_rows,
};

/// A note's mention of one row: where, and whether it withdraws it.
struct Mention {
    path: String,
    line: usize,
    package: String,
    clause: String,
    clean: String,
    stale: bool,
}

fn mirror_rows(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let finding = |path: &str, line: usize, detail: String| Finding {
        rule: MIRROR_ROWS.id,
        path: path.to_string(),
        line,
        detail,
    };
    let rows = match tree.text(REGISTRY).map(|t| mirror::parse(&t)) {
        Some(Ok(rows)) => rows,
        Some(Err(why)) => {
            out.push(finding(REGISTRY, 0, why));
            return;
        }
        None => Vec::new(),
    };
    let mut mentions: BTreeMap<String, Vec<Mention>> = BTreeMap::new();
    let mut specs_by_pkg: BTreeMap<String, String> = BTreeMap::new();
    for (manifest, _) in tree.corpus.manifests() {
        let Some(package) = manifest.strip_suffix("/Cargo.toml") else {
            continue;
        };
        let specs_path = format!("{package}/SPECS.md");
        let Some(specs) = tree.text(&specs_path).map(Cow::into_owned) else {
            continue;
        };
        let clauses = mirror::clauses(&specs);
        let rows_at: BTreeMap<usize, &mirror::Clause<'_>> =
            clauses.iter().map(|c| (c.line, c)).collect();
        for (index, line) in specs.lines().enumerate() {
            let at = index + 1;
            let mut check_cell = |text: &str, notes_cell: bool| {
                let (notes, bad) = mirror::notes(text);
                for why in bad {
                    out.push(finding(&specs_path, at, why));
                }
                if notes.is_empty() {
                    return;
                }
                let Some(clause) = rows_at.get(&at).filter(|_| notes_cell) else {
                    out.push(finding(
                        &specs_path,
                        at,
                        format!(
                            "a \"{CLAIM}\" or \"{STALE}\" note stands outside a clause row's \
                             notes cell, where no row is held to it: move it to the notes \
                             (gaps) column of the clause it proves"
                        ),
                    ));
                    return;
                };
                for note in notes {
                    for name in note.rows {
                        mentions.entry(name).or_default().push(Mention {
                            path: specs_path.clone(),
                            line: at,
                            package: package.to_string(),
                            clause: clause.id.clone(),
                            clean: note.clean.clone(),
                            stale: note.stale,
                        });
                    }
                }
            };
            match rows_at.get(&at) {
                Some(clause) => {
                    for c in &clause.cells {
                        check_cell(c.text, c.notes);
                    }
                }
                None => check_cell(line, false),
            }
        }
        specs_by_pkg.insert(package.to_string(), specs);
    }
    let manifests = || {
        tree.corpus
            .manifests()
            .iter()
            .map(|(r, t)| (r.as_str(), t.as_str()))
    };
    let by_name: BTreeMap<&str, &Row> = rows.iter().map(|r| (r.row.as_str(), r)).collect();
    for (name, said) in &mentions {
        for m in said {
            let Some(row) = by_name.get(name.as_str()) else {
                out.push(finding(
                    &m.path,
                    m.line,
                    format!(
                        "the note names Clean {} row `{name}`, which {REGISTRY} does not \
                         record: record it (`aterm-census mirror record`, then `mirror \
                         audit`), or drop the note",
                        m.clean
                    ),
                ));
                continue;
            };
            let package = mirror::package_dir(manifests(), &row.krate);
            if row.clean != m.clean
                || package.as_deref() != Some(m.package.as_str())
                || row.clause != m.clause
            {
                out.push(finding(
                    &m.path,
                    m.line,
                    format!(
                        "the note names Clean {} row `{name}` on {}'s clause `{}`; {REGISTRY} \
                         records it as Clean {} on {}'s clause `{}`",
                        m.clean, m.package, m.clause, row.clean, row.krate, row.clause
                    ),
                ));
            }
        }
    }
    let mut parsed_files: BTreeMap<&str, Option<mirror::item::ParsedFile>> = BTreeMap::new();
    let mut item_digests: BTreeMap<(&str, &str), Result<String, String>> = BTreeMap::new();
    let clauses_by_pkg: BTreeMap<&str, Vec<mirror::Clause<'_>>> = specs_by_pkg
        .iter()
        .map(|(pkg, specs)| (pkg.as_str(), mirror::clauses(specs)))
        .collect();
    for row in &rows {
        let unreadable = |why: String| {
            finding(
                REGISTRY,
                0,
                format!(
                    "{} row `{}` cannot be read ({why}): record it where its items and clause \
                     live now (`aterm-census mirror reproved` once Clean proves it there), or \
                     `aterm-census mirror forget {}` and drop its note",
                    row.clean, row.row, row.row
                ),
            )
        };
        let Some(package) = mirror::package_dir(manifests(), &row.krate) else {
            out.push(unreadable(format!(
                "no package `{}` under crates/ or apps/",
                row.krate
            )));
            continue;
        };
        let said: Vec<&Mention> = mentions
            .get(&row.row)
            .map_or(&[][..], Vec::as_slice)
            .iter()
            .filter(|m| m.package == package && m.clause == row.clause)
            .collect();
        if row.clause != NO_CLAUSE && said.len() != 1 {
            let specs_path = format!("{package}/SPECS.md");
            out.push(finding(
                &specs_path,
                said.first().map_or(0, |m| m.line),
                format!(
                    "{} row `{}` is named by {} notes, not one: clause `{}`'s notes cell \
                     says \"{CLAIM}: Clean {} row `{}`\" while it holds, \"{STALE}: …\" once \
                     it does not",
                    row.clean,
                    row.row,
                    said.len(),
                    row.clause,
                    row.clean,
                    row.row
                ),
            ));
        }
        let stale = row.clause != NO_CLAUSE && said.len() == 1 && said[0].stale;
        let now = reading_cached(
            tree,
            row,
            &package,
            &clauses_by_pkg,
            &mut parsed_files,
            &mut item_digests,
        );
        let now = match now {
            Ok(now) => now,
            Err(why) => {
                out.push(unreadable(why));
                continue;
            }
        };
        let moved = mirror::moved(row, &now);
        if moved.is_empty() || stale {
            continue;
        }
        let (path, line) = match (now.clause_line, said.first()) {
            (Some(line), _) => (format!("{package}/SPECS.md"), line),
            (None, Some(m)) => (m.path.clone(), m.line),
            (None, None) => (REGISTRY.to_string(), 0),
        };
        let fix = if row.clause == NO_CLAUSE {
            format!("or `aterm-census mirror forget {}`", row.row)
        } else {
            format!(
                "or change its note to \"{STALE}: Clean {} row `{}`\"",
                row.clean, row.row
            )
        };
        out.push(finding(
            &path,
            line,
            format!(
                "{} row `{}` proved a copy that is no longer the code: {}. Prove it again in \
                 Clean and run `aterm-census mirror reproved --clean <R> --aterm <sha> {}`, {fix}",
                row.clean,
                row.row,
                moved.join("; "),
                row.row
            ),
        ));
    }
}

fn reading_cached<'t>(
    tree: &'t Tree<'_>,
    row: &'t Row,
    package: &str,
    clauses_by_pkg: &BTreeMap<&str, Vec<mirror::Clause<'_>>>,
    parsed_files: &mut BTreeMap<&'t str, Option<mirror::item::ParsedFile>>,
    item_digests: &mut BTreeMap<(&'t str, &'t str), Result<String, String>>,
) -> Result<mirror::Reading, String> {
    let mut digests = Vec::with_capacity(row.items.len());
    for item in &row.items {
        let key = (item.file.as_str(), item.path.as_str());
        if let Some(cached) = item_digests.get(&key) {
            digests.push(cached.clone()?);
            continue;
        }
        let parsed = parsed_files.entry(item.file.as_str()).or_insert_with(|| {
            tree.text(&item.file)
                .map(|src| mirror::item::ParsedFile::of(&src))
        });
        let res = match parsed {
            Some(p) => {
                mirror::item_digest_in(p, &item.path).map_err(|e| format!("{}: {e}", item.file))
            }
            None => Err(format!("{} does not exist", item.file)),
        };
        item_digests.insert(key, res.clone());
        digests.push(res?);
    }
    if row.clause == NO_CLAUSE {
        return Ok(mirror::Reading {
            digests,
            clause_digest: NO_CLAUSE.to_string(),
            clause_line: None,
        });
    }
    let specs_path = format!("{package}/SPECS.md");
    let clauses = clauses_by_pkg
        .get(package)
        .ok_or_else(|| format!("{specs_path} does not exist"))?;
    let mut found = clauses.iter().filter(|c| c.id == row.clause);
    let Some(clause) = found.next() else {
        return Err(format!("{specs_path} has no clause `{}`", row.clause));
    };
    if let Some(again) = found.next() {
        return Err(format!(
            "{specs_path}:{} gives clause `{}` a second row: one id, one clause",
            again.line, row.clause
        ));
    }
    Ok(mirror::Reading {
        digests,
        clause_digest: clause.digest(),
        clause_line: Some(clause.line),
    })
}

#[cfg(test)]
#[path = "mirror_rows_tests.rs"]
mod tests;
