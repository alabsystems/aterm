// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The owner's grid edit (docs/DESIGN-alab-lab-model-2026-11-01.md §8): one collection written
//! as a TSV to a private scratch folder, opened in amacs's spreadsheet mode and waited on, then
//! read back and written cell by cell. A file the save cannot take (a bad value, a changed
//! read-only cell) reopens unchanged with the caret on the first bad row and nothing written; a
//! cell that changed meanwhile reopens as `⚠ mine ‖ theirs`, the rest written. Closing a
//! reopened file unchanged ends the session. The scratch folder goes when the session ends,
//! however it ends. [`show`] opens the same grid read-only and writes nothing back.

use super::{Applied, Handle};
use crate::{
    Error, Result,
    alab::secret::{Journal, Surface},
    typed::{
        Collection, Ty,
        cell::{Cells, Field},
        check,
        grid::{self, Base, Opened, Problem, Where},
    },
};
use std::path::Path;

/// A `Secret` cell [`open_secrets`] asks for, by row key and column, with its candidates: a
/// token to open, or `None` for a cleared one.
type Wanted = ((String, &'static str), Vec<Option<String>>);

/// What a grid with its `Secret` columns revealed and editable needs (the owner's `--edit`,
/// docs/DESIGN-alab-one-mesh-2026-11-05.md §4.8): the journal each reveal is recorded in first,
/// and the clock it records.
pub struct GridSecrets<'a> {
    pub journal: &'a Journal,
    pub when_ms: u64,
}

/// Every set `Secret` cell of `collection` in `cells` (of the column `only`, or of each live
/// `Secret` column), revealed through the audited exit ([`Handle::reveal_cells`]) as `surface`:
/// by row and column, each candidate of a conflict, a cleared candidate as an empty value beside
/// the others. A protected row's cell is never asked. A cell any of whose candidates does not
/// open here (no key of its epoch, a token that does not authenticate, a value that is no token)
/// is left out, so a grid shows it masked and read-only and a view shows it masked; the
/// candidates of that cell that did open are still journaled as revealed (the journal may name
/// more than was shown, never less).
///
/// # Errors
/// As [`Handle::reveal_cells`].
pub fn open_secrets(
    handle: &Handle,
    collection: &'static Collection,
    only: Option<&str>,
    cells: &Cells,
    journal: &Journal,
    surface: Surface,
    when_ms: u64,
) -> Result<Opened> {
    let projection = crate::typed::cell::project(collection, cells);
    let mut wanted: Vec<Wanted> = Vec::new();
    for column in collection.columns.iter().filter(|c| {
        matches!(c.ty, Ty::Secret)
            && c.live_at(collection.version)
            && only.is_none_or(|only| only == c.name)
    }) {
        for (pk, row) in &projection.rows {
            if check::protected(row) {
                continue;
            }
            let values: Vec<&aterm_json::Value> = match row.fields.get(column.name) {
                Some(Field::Value(candidate)) => vec![&candidate.value.x],
                Some(Field::Conflict { candidates, .. }) => {
                    candidates.iter().map(|c| &c.value.x).collect()
                }
                Some(Field::Set(_)) | None => Vec::new(),
            };
            if values.iter().all(|value| value.is_null()) {
                continue;
            }
            let candidates: Option<Vec<Option<String>>> = values
                .into_iter()
                .map(|value| match value {
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
    let mut values = handle
        .reveal_cells(&asked, journal, surface, when_ms)?
        .into_iter();
    let mut opened = Opened::new();
    for ((pk, column), candidates) in wanted {
        let mut shown = Vec::with_capacity(candidates.len());
        let mut whole = true;
        for candidate in &candidates {
            match candidate {
                None => shown.push(crate::typed::secret::Secret::new(String::new())),
                Some(_) => match values.next().flatten() {
                    Some(value) => shown.push(value),
                    None => whole = false,
                },
            }
        }
        if whole {
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
/// 1-based line, the headers of its `Secret` columns named to the window as concealed
/// ([`grid::secret_headers`]; `edit … secret=`): no reader of the window but the person's eyes
/// gets their cells (docs/DESIGN-alab-one-mesh-2026-11-05.md §4.8).
pub type Opener<'a> = &'a mut dyn FnMut(&Path, usize, &[String]) -> Result<()>;

/// The production opener: `aterm edit --wait [--secret=…] +<line> <file>` from this same
/// executable, which inside aterm opens amacs in an Editor tab beside this one (a `.tsv` opens
/// in its spreadsheet mode) and elsewhere edits in this terminal. `secret`, where given, names
/// the grid's concealed columns ([`Opener`]); a private file opened with none named conceals
/// every cell.
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

/// The production opener in a person's aterm tab: `file` in an Editor tab beside session `sid`
/// (the tab the reveal was allowed in, by the `reveal` answer, never by the environment), through
/// the window's `edit` verb, read-only with `ro=1`, its `secret` columns concealed
/// (`secret=`, [`Opener`]); `opened` hears the window's answer once the tab is open, and this
/// returns once it closes. It never edits in the terminal instead: plaintext
/// drawn there would stay in that tab's scrollback, blocks, recordings and session log
/// (docs/DESIGN-alab-one-mesh-2026-11-05.md §4.8), so a window that refuses ends the command.
///
/// # Errors
/// The file has no `file://` URI, the window refused or could not be asked, or its close was not
/// heard.
pub fn in_editor_tab(
    sid: &str,
    file: &Path,
    line: usize,
    read_only: bool,
    secret: &[String],
    opened: &mut dyn FnMut(&aterm_ctl::edit::EditOk),
) -> Result<()> {
    use aterm_ctl::edit::{AwaitKey, EditRequest};
    let uri = amacs::protocol::uri::path_to_file_uri(file)
        .map_err(|_| Error::new("the grid file has no file URI"))?;
    let request = EditRequest {
        uri,
        line: Some((u32::try_from(line.max(1)).unwrap_or(1), None)),
        create: false,
        for_word: Some(EDIT_FOR.to_owned()),
        wait: true,
        read_only,
        secret: Some(secret.to_vec()),
    };
    let ok = aterm_ctl::edit::edit(sid, &request).map_err(|error| {
        Error::new(&format!(
            "the Editor tab did not open ({error}); in an aterm tab the grid opens only there, \
             never in the terminal"
        ))
    })?;
    opened(&ok);
    aterm_ctl::edit::await_edit(sid, AwaitKey::Ticket(ok.ticket), &ok.uri)
        .map_err(|error| Error::new(&format!("the Editor tab's close was not heard ({error})")))?;
    Ok(())
}

/// The program an Editor tab's header names for a grid (`edit … for=`).
const EDIT_FOR: &str = "aterm-db";

/// Show `collection` of `handle`'s table read-only: the grid [`edit`] opens, its `Secret` columns
/// revealed (each reveal journaled first, as [`Surface::Grid`]) and addresses shown, written to a
/// private scratch folder, opened by `open` (read-only) and removed once `open` returns. Nothing
/// is read back or written. Returns the rows shown.
///
/// # Errors
/// The table cannot be read, the scratch folder cannot be made, a reveal fails, or `open` fails.
pub fn show(
    handle: &Handle,
    collection: &'static Collection,
    secrets: &GridSecrets<'_>,
    open: Opener<'_>,
) -> Result<usize> {
    let compiled = handle.compiled();
    let scratch = scratch()?;
    let cells = handle.view()?.cells;
    let opened = open_secrets(
        handle,
        collection,
        None,
        &cells,
        secrets.journal,
        Surface::Grid,
        secrets.when_ms,
    )?;
    let exported =
        grid::export_opened(compiled.table, collection, &cells, &[], true, Some(&opened))?;
    let rows = exported.base.rows.len();
    let text = zeroize::Zeroizing::new(exported.text);
    let name = format!("{}.tsv", collection.name);
    aterm_uds::private_file::write_private(scratch.path(), &name, text.as_bytes())
        .map_err(|_| Error::new("the grid file could not be written"))?;
    open(
        &scratch.path().join(&name),
        1,
        &grid::secret_headers(collection, true),
    )?;
    drop(scratch);
    Ok(rows)
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
/// addresses masked unless `reveal`, its `Secret` columns revealed and editable where
/// `secrets` is given (each export journals each reveal first) and masked and read-only where
/// not. `open` shows the file and returns once it is closed; `report` hears each problem and
/// stale cell, by row and column, never a value. The file lives in a private folder amacs keeps
/// no draft of and serves no `doc read` of ([`aterm_types::private_edit`]), removed when the
/// session ends; one a killed session left is swept at the next.
///
/// # Errors
/// The table cannot be read or written, the scratch folder cannot be made, a reveal fails, or
/// `open` fails.
pub fn edit(
    handle: &Handle,
    collection: &'static Collection,
    filters: &[Where],
    reveal: bool,
    secrets: Option<&GridSecrets<'_>>,
    open: Opener<'_>,
    report: &mut dyn FnMut(&str),
) -> Result<Edited> {
    let compiled = handle.compiled();
    let scratch = scratch()?;
    let export = |cells: &Cells, filters: &[Where]| -> Result<grid::Export> {
        let opened = secrets
            .map(|secrets| {
                open_secrets(
                    handle,
                    collection,
                    None,
                    cells,
                    secrets.journal,
                    Surface::Grid,
                    secrets.when_ms,
                )
            })
            .transpose()?;
        grid::export_opened(
            compiled.table,
            collection,
            cells,
            filters,
            reveal,
            opened.as_ref(),
        )
    };
    let name = format!("{}.tsv", collection.name);
    let path = scratch.path().join(&name);
    let secret = grid::secret_headers(collection, secrets.is_some());
    let write = |text: &str| {
        aterm_uds::private_file::write_private(scratch.path(), &name, text.as_bytes())
            .map_err(|_| Error::new("the grid file could not be written"))
    };
    let mut exported = export(&handle.view()?.cells, filters)?;
    let mut presented = zeroize::Zeroizing::new(exported.text.clone());
    write(&presented)?;
    let mut line = 1;
    let mut edited = Edited::default();
    loop {
        open(&path, line, &secret)?;
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
                    export(&cells, &[])?.base
                } else {
                    let matching = export(&cells, filters)?;
                    let mut keep: std::collections::BTreeSet<&str> = matching
                        .base
                        .rows
                        .iter()
                        .map(|(pk, _)| pk.as_str())
                        .collect();
                    keep.extend(exported.base.rows.iter().map(|(pk, _)| pk.as_str()));
                    keep.extend(plan.born.iter().map(String::as_str));
                    keep.extend(stale.iter().map(|item| item.pk.as_str()));
                    let mut all = export(&cells, &[])?.base;
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
