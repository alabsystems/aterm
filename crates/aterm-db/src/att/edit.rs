// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The owner's grid edit (docs/DESIGN-alab-lab-model-2026-11-01.md §8): one collection written
//! as a TSV to a private scratch folder, opened in amacs's spreadsheet mode and waited on, then
//! read back and written cell by cell. A file the save cannot take (a bad value, a changed
//! read-only cell) reopens unchanged with the caret on the first bad row and nothing written; a
//! cell that changed meanwhile reopens as `⚠ mine ‖ theirs`, the rest written. Closing a
//! reopened file unchanged ends the session. The scratch folder goes when the session ends,
//! however it ends.

use super::{Applied, Handle};
use crate::{
    Error, Result,
    typed::{
        Collection,
        grid::{self, Base, Problem, Where},
    },
};
use std::path::Path;

/// Opens a grid file for the owner and returns once they have closed it, the caret on a
/// 1-based line.
pub type Opener<'a> = &'a mut dyn FnMut(&Path, usize) -> Result<()>;

/// The production opener: `aterm edit --wait +<line> <file>` from this same executable, which
/// inside aterm opens amacs in an Editor tab beside this one (a `.tsv` opens in its spreadsheet
/// mode) and elsewhere edits in this terminal.
///
/// # Errors
/// This executable cannot be found or started, or amacs did not open the file.
pub fn amacs(file: &Path, line: usize) -> Result<()> {
    let exe =
        std::env::current_exe().map_err(|_| Error::new("this executable's path is unknown"))?;
    let status = aterm_uds::spawnfd::command(exe)
        .arg("edit")
        .arg("--wait")
        .arg(format!("+{line}"))
        .arg(file)
        .status()
        .map_err(|_| Error::new("amacs could not be started"))?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::new(
            "amacs did not open the file (it needs a terminal or an aterm window)",
        ))
    }
}

/// What an edit session did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Edited {
    /// Times the file was opened.
    pub rounds: usize,
    /// Each save's writes.
    pub applied: Vec<Applied>,
    /// The owner closed a file whose problems were still there: nothing of it was written.
    pub abandoned: bool,
    /// The last round's problems.
    pub problems: Vec<Problem>,
}

/// Where a problem sits, in words: the data row and the column's name.
pub fn place(base: &Base, problem: &Problem) -> String {
    let column = base
        .header
        .get(problem.column)
        .map_or("past the last column", |h| {
            h.trim_start_matches(grid::READ_ONLY)
        });
    if problem.record == 0 {
        format!("header, column {column}")
    } else {
        format!("row {}, column {column}", problem.record)
    }
}

fn read_back(path: &Path) -> Result<String> {
    super::read_regular_utf8(
        path,
        grid::MAX_BYTES,
        "the grid file could not be read back",
        "the grid file is larger than the grid reads",
        "the grid file is not UTF-8",
    )
}

/// Edit `collection` of `handle`'s table in the grid: the rows every filter matches,
/// addresses masked unless `reveal`. `open` shows the file and returns once it is closed;
/// `report` hears each problem and stale cell, by row and column, never a value.
///
/// # Errors
/// The table cannot be read or written, the scratch folder cannot be made, or `open` fails.
pub fn edit(
    handle: &Handle,
    collection: &'static Collection,
    filters: &[Where],
    reveal: bool,
    open: Opener<'_>,
    report: &mut dyn FnMut(&str),
) -> Result<Edited> {
    let compiled = handle.compiled();
    let scratch = aterm_tempfile::Builder::new()
        .prefix("att-edit-")
        .suffix(".noindex")
        .tempdir()
        .map_err(|_| Error::new("the grid's private scratch folder could not be made"))?;
    let name = format!("{}.tsv", collection.name);
    let path = scratch.path().join(&name);
    let write = |text: &str| {
        aterm_uds::private_file::write_private(scratch.path(), &name, text.as_bytes())
            .map_err(|_| Error::new("the grid file could not be written"))
    };
    let mut export = grid::export(
        compiled.table,
        collection,
        &handle.view()?.cells,
        filters,
        reveal,
    )?;
    let mut presented = export.text.clone();
    write(&presented)?;
    let mut line = 1;
    let mut edited = Edited::default();
    loop {
        open(&path, line)?;
        edited.rounds += 1;
        let saved = read_back(&path)?;
        if saved == presented {
            edited.abandoned = !edited.problems.is_empty();
            break;
        }
        let view = handle.view()?;
        let birth = |c: &Collection, values: &_| compiled.born(c, values, &view.cells);
        match grid::plan(compiled.table, collection, &export.base, &saved, &birth) {
            Err(problems) => {
                for problem in &problems {
                    report(&format!(
                        "{}: {}",
                        place(&export.base, problem),
                        problem.rule
                    ));
                }
                line = problems.first().map_or(1, |p| p.line);
                edited.problems = problems;
                presented = saved;
            }
            Ok(plan) => {
                edited.problems.clear();
                let applied = handle.apply(collection, &plan)?;
                let stale = applied.stale.clone();
                edited.applied.push(applied);
                if stale.is_empty() {
                    break;
                }
                for item in &stale {
                    report(&format!(
                        "row {}, column {}: changed since the export; it shows both values, \
                         yours first",
                        super::command::safe_key(&item.pk),
                        super::command::safe_key(&item.column),
                    ));
                }
                let cells = handle.view()?.cells;
                let fresh = if filters.is_empty() {
                    grid::export(compiled.table, collection, &cells, &[], reveal)?.base
                } else {
                    let matching =
                        grid::export(compiled.table, collection, &cells, filters, reveal)?;
                    let mut keep: std::collections::BTreeSet<&str> = matching
                        .base
                        .rows
                        .iter()
                        .map(|(pk, _)| pk.as_str())
                        .collect();
                    keep.extend(export.base.rows.iter().map(|(pk, _)| pk.as_str()));
                    keep.extend(plan.born.iter().map(String::as_str));
                    keep.extend(stale.iter().map(|item| item.pk.as_str()));
                    let mut all =
                        grid::export(compiled.table, collection, &cells, &[], reveal)?.base;
                    all.rows.retain(|(pk, _)| keep.contains(pk.as_str()));
                    all
                };
                let base = grid::reopen(fresh, &stale);
                let text = grid::render(&base);
                line = grid::stale_line(&text, &base, &stale);
                presented = text.clone();
                export = grid::Export { text, base };
                write(&presented)?;
            }
        }
    }
    drop(scratch);
    Ok(edited)
}
