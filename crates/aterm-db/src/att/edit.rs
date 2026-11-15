// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The owner's grid edit (docs/DESIGN-alab-lab-model-2026-11-01.md §8): one collection written
//! as a TSV to a private scratch folder, opened in amacs's spreadsheet mode and waited on, then
//! read back and written cell by cell. A file the save cannot take (a bad value, a changed
//! read-only cell, a save the table's rules refuse) reopens unchanged with the caret on the
//! first bad row and nothing written; a cell that changed meanwhile reopens as `⚠ mine ‖
//! theirs`, the rest written. Closing a reopened file unchanged ends the session. The scratch
//! folder goes when the session ends, however it ends. No cell shows a secret: a secret column
//! that holds a reference to a secret-store entry shows its entry name, one that holds a sealed
//! token (a generation builds up to 0.200.0 read) shows the mask, and both are read-only. A
//! column such a generation holds sealed though its values are ordinary data (a phone's
//! `number`) shows each value, opened for the grid ([`open_ordinary`]), read-only too.

use super::{Applied, Handle, Refused};
use crate::{
    Error, Result,
    typed::{
        Collection,
        cell::{Cells, Field},
        check,
        grid::{self, Base, Opened, Plan, Problem, Where},
        secret::Secret,
    },
};
use std::path::Path;

/// A sealed cell [`open_ordinary`] asks for, by row key and column, with its candidates: a
/// token to open, or `None` for a cleared one.
type Wanted = ((String, &'static str), Vec<Option<String>>);

/// What opens the cells [`open_with`] asks for: one value per asked
/// `(collection, row, column, token)`, in order, `None` where it did not open.
type OpenCells<'a> = &'a mut dyn FnMut(
    &[(&str, &str, &str, &str)],
) -> Result<Vec<Option<crate::typed::secret::Secret>>>;

/// Every set cell of `column` of `collection` in `cells`, a column whose values are ordinary data
/// that the generation `handle` reaches holds sealed
/// ([`crate::typed::Column::sealed_ordinary`]: a phone's `number`, which builds up to 0.200.0
/// read only as a sealed token): opened for every reader of the table, with nothing journaled,
/// since no secret is revealed. By row and column, each candidate of a conflict, a cleared
/// candidate as an empty value beside the others. A cell any of whose candidates does not open
/// here (no key of its epoch, a token that does not authenticate, a value that is no token) is
/// left out, and so is one any of whose opened values does not read as a phone number
/// ([`aterm_value_guard::stored_phone`]): what a build up to 0.200.0 sealed there under a
/// secret's contract is never shown this way, and a view shows either cell masked. A secret's
/// column (a password, a TOTP seed) opens nothing here, whatever `column` names. No cell asked
/// asks nothing of the store; a store that is busy, or changed under the read, is observed again
/// until the admission deadline ([`super::reopened`]: the read writes nothing).
///
/// # Errors
/// The store cannot be read.
pub fn open_ordinary(
    handle: &Handle,
    collection: &'static Collection,
    column: &str,
    cells: &Cells,
) -> Result<Opened> {
    open_with(collection, Some(column), cells, &mut |asked| {
        super::reopened(crate::admission::deadline(), || handle.open_cells(asked))
    })
}

/// [`open_ordinary`] for every such column of `collection`: what a grid of the collection shows
/// in place of the mask. Empty, and no store session opened, for a collection without one (every
/// collection of a generation whose numbers are plain).
///
/// # Errors
/// The store cannot be read.
pub fn open_every_ordinary(
    handle: &Handle,
    collection: &'static Collection,
    cells: &Cells,
) -> Result<Opened> {
    if !collection
        .columns
        .iter()
        .any(|column| column.sealed_ordinary() && column.live_at(collection.version))
    {
        return Ok(Opened::new());
    }
    open_with(collection, None, cells, &mut |asked| {
        super::reopened(crate::admission::deadline(), || handle.open_cells(asked))
    })
}

/// The cells [`open_ordinary`] asks for (of the column `only`, or of each such column), opened
/// by `open`: one value per asked `(collection, row, column, token)`, in order, `None` where it
/// did not open.
fn open_with(
    collection: &'static Collection,
    only: Option<&str>,
    cells: &Cells,
    open: OpenCells<'_>,
) -> Result<Opened> {
    let projection = crate::typed::cell::project(collection, cells);
    let mut wanted: Vec<Wanted> = Vec::new();
    for column in collection.columns.iter().filter(|c| {
        c.sealed_ordinary()
            && c.live_at(collection.version)
            && only.is_none_or(|only| only == c.name)
    }) {
        for (pk, row) in &projection.rows {
            if check::protected(row) {
                continue;
            }
            let candidates_slice: &[crate::typed::cell::Candidate] =
                match row.fields.get(column.name) {
                    Some(Field::Value(candidate)) => std::slice::from_ref(candidate),
                    Some(Field::Conflict { candidates, .. }) => candidates.as_slice(),
                    Some(Field::Set(_)) | None => &[],
                };
            if candidates_slice.iter().all(|c| c.value.x.is_null()) {
                continue;
            }
            let candidates: Option<Vec<Option<String>>> = candidates_slice
                .iter()
                .map(|c| match &c.value.x {
                    aterm_json::Value::Null => Some(None),
                    aterm_json::Value::String(token) if crate::typed::secret::is_sealed(token) => {
                        Some(Some(token.clone()))
                    }
                    _ => None,
                })
                .collect();
            if let Some(candidates) = candidates {
                wanted.push(((pk.clone(), column.name), candidates));
            }
        }
    }
    let asked: Vec<(&str, &str, &str, &str)> = wanted
        .iter()
        .flat_map(|((pk, column), candidates)| {
            candidates
                .iter()
                .flatten()
                .map(move |token| (collection.name, pk.as_str(), *column, token.as_str()))
        })
        .collect();
    if asked.is_empty() {
        return Ok(Opened::new());
    }
    let mut values = open(&asked)?.into_iter();
    let mut opened = Opened::new();
    for ((pk, column), candidates) in wanted {
        let mut shown = Vec::with_capacity(candidates.len());
        let mut whole = true;
        for candidate in &candidates {
            match candidate {
                None => shown.push(Secret::new(String::new())),
                Some(_) => match values.next().flatten() {
                    Some(value) => shown.push(value),
                    None => whole = false,
                },
            }
        }
        // What a build up to 0.200.0 sealed here under a secret's contract (any `--stdin` or
        // grid text) is never shown this way: only what reads as a phone number is kept.
        let numbers = shown.iter().all(|value| {
            let text = value.plaintext();
            text.is_empty() || aterm_value_guard::stored_phone(text)
        });
        if whole && numbers {
            opened.insert((pk, column.to_owned()), shown);
        }
    }
    Ok(opened)
}

/// A private scratch folder for one grid ([`aterm_types::private_edit`]): `0700`, named for this
/// process in [`aterm_types::private_edit::scratch_root`], removed when dropped. Each folder a
/// killed edit left beside it is swept first.
fn scratch() -> Result<aterm_tempfile::TempDir> {
    let scratch = aterm_tempfile::Builder::new()
        .prefix(aterm_types::private_edit::SCRATCH_PREFIX)
        .suffix(aterm_types::private_edit::DIR_SUFFIX)
        .tempdir_in(aterm_types::private_edit::scratch_root())
        .map_err(|_| Error::new("the grid's private scratch folder could not be made"))?;
    if let Some(parent) = scratch.path().parent() {
        aterm_types::private_edit::sweep_dead(parent, Some(scratch.path()));
    }
    Ok(scratch)
}

/// Opens a grid file for the owner and returns once they have closed it, the caret on a
/// 1-based line.
pub type Opener<'a> = &'a mut dyn FnMut(&Path, usize) -> Result<()>;

/// `aterm edit --wait [--secret=…] +<line> <file>` from this same executable, which inside
/// aterm opens amacs in an Editor tab beside this one (a `.tsv` opens in its spreadsheet mode)
/// and elsewhere edits in this terminal. `secret`, where given, names the columns the window
/// conceals (`Some(&[])` names none, as a grid's opener does: no grid cell shows a secret); a
/// private file opened with `None` conceals every cell.
///
/// # Errors
/// This executable cannot be found or started, or amacs did not open the file.
pub fn amacs(file: &Path, line: usize, secret: Option<&[String]>) -> Result<()> {
    let exe =
        std::env::current_exe().map_err(|_| Error::new("this executable's path is unknown"))?;
    let mut command = aterm_uds::spawnfd::command(exe);
    command.arg("edit").arg("--wait");
    if let Some(secret) = secret {
        command.arg(format!("--secret={}", secret.join(",")));
    }
    let status = command
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

/// The problem a save the table's rules refused is ([`Applied::refused`]): at the record and
/// grid column of the refused row's edit of that column, else of its first edit, else the
/// header's.
pub(crate) fn refused_problem(plan: &Plan, refused: &Refused) -> Problem {
    let edits = plan
        .rows
        .iter()
        .find(|(pk, _)| *pk == refused.pk)
        .map_or(&[][..], |(_, edits)| edits.as_slice());
    let edit = edits
        .iter()
        .find(|edit| edit.column == refused.column)
        .or_else(|| edits.first());
    Problem {
        record: edit.map_or(0, |edit| edit.record),
        column: edit.map_or(0, |edit| edit.at),
        line: edit.map_or(1, |edit| edit.line),
        rule: refused.problem_rule(),
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

/// Edit `collection` of `handle`'s table in the grid: the rows every filter matches, each value
/// as it is stored, a secret reference by its entry name, a sealed secret as the mask and a
/// number a generation holds sealed as it is ([`open_every_ordinary`]), those three read-only.
/// `open` shows the file and returns once it is closed; `report` hears each problem and stale
/// cell, by row and column, never a value. The file lives in a private folder amacs keeps
/// no draft of and serves no `doc read` of ([`aterm_types::private_edit`]), removed when the
/// session ends; one a killed session left is swept at the next.
///
/// # Errors
/// The table cannot be read or written, the scratch folder cannot be made, or `open` fails.
pub fn edit(
    handle: &Handle,
    collection: &'static Collection,
    filters: &[Where],
    open: Opener<'_>,
    report: &mut dyn FnMut(&str),
) -> Result<Edited> {
    let compiled = handle.compiled();
    let scratch = scratch()?;
    let export = |view: &super::View, filters: &[Where]| -> Result<grid::Export> {
        Ok(view
            .export_grid(compiled.table, collection, filters)?
            .showing(&open_every_ordinary(handle, collection, &view.cells)?))
    };
    let name = format!("{}.tsv", collection.name);
    let path = scratch.path().join(&name);
    let write = |text: &str| {
        aterm_uds::private_file::write_private(scratch.path(), &name, text.as_bytes())
            .map_err(|_| Error::new("the grid file could not be written"))
    };
    let mut exported = export(&handle.view()?, filters)?;
    let mut presented = zeroize::Zeroizing::new(exported.text.clone());
    write(&presented)?;
    let mut line = 1;
    let mut edited = Edited::default();
    loop {
        open(&path, line)?;
        edited.rounds += 1;
        let saved = zeroize::Zeroizing::new(read_back(&path)?);
        if *saved == *presented {
            edited.abandoned = !edited.problems.is_empty();
            break;
        }
        let view = handle.view()?;
        let birth = |c: &Collection, values: &_| compiled.born(c, values, &view.cells);
        match grid::plan(compiled.table, collection, &exported.base, &saved, &birth) {
            Err(problems) => {
                for problem in &problems {
                    report(&format!(
                        "{}: {}",
                        place(&exported.base, problem),
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
                if let Some(refused) = &applied.refused {
                    let problem = refused_problem(&plan, refused);
                    report(&format!(
                        "{}: {}",
                        place(&exported.base, &problem),
                        problem.rule
                    ));
                    line = problem.line;
                    edited.problems = vec![problem];
                    presented = saved;
                    continue;
                }
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
                let view = handle.view()?;
                let fresh = if filters.is_empty() {
                    export(&view, &[])?.base
                } else {
                    let matching = export(&view, filters)?;
                    let mut keep: std::collections::BTreeSet<&str> = matching
                        .base
                        .rows
                        .iter()
                        .map(|(pk, _)| pk.as_str())
                        .collect();
                    keep.extend(exported.base.rows.iter().map(|(pk, _)| pk.as_str()));
                    keep.extend(plan.born.iter().map(String::as_str));
                    keep.extend(stale.iter().map(|item| item.pk.as_str()));
                    let mut all = export(&view, &[])?.base;
                    all.rows.retain(|(pk, _)| keep.contains(pk.as_str()));
                    all
                };
                let base = grid::reopen(fresh, &stale);
                let text = grid::render(&base);
                line = grid::stale_line(&text, &base, &stale);
                presented = zeroize::Zeroizing::new(text.clone());
                exported = grid::Export { text, base };
                write(&presented)?;
            }
        }
    }
    drop(scratch);
    Ok(edited)
}
