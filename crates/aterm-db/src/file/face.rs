// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Editing a shared file's face (docs/DESIGN-shared-files-2026-11-11.md §3, "Editing a face"
//! and "What a commit does"; slice SF-06).
//!
//! A face is the TSV a pass writes from the snapshot, its last header cell `=pk@<rev>`: the
//! revision it was written at. Beside each revision's snapshot a pass keeps that revision's
//! [`Baseline`]: what every cell of its face showed, and the candidates each cell's register
//! held (the grid's own [`grid::Base`]). Nothing reads a face but [`commit`], which the verb
//! `aterm file commit <name>` runs by name (and `edit`, and the hook the `aterm` executable
//! installs for a save in an Editor tab); no watcher, pass or publisher takes face bytes.
//!
//! A commit reads the face against the baseline its header names, never against a later one:
//!
//! - only a cell whose text differs from what that revision's face showed becomes a write, and
//!   it carries the candidates that revision saw, so a register that moved since is held, never
//!   overwritten;
//! - a cell whose text this Mac's own queued or held writes already say asks for nothing, and so
//!   does a new row this Mac already asked for (a stale copy of the face saved again);
//! - a row missing from the face changes nothing ([`grid::Absent::Keeps`]): a row is retired by
//!   its `row` cell or by `aterm file rm`;
//! - a new row with its key left empty is born under the key the publisher derives, all its
//!   cells in one intent;
//! - a bad value queues nothing and says where in `<leaf>.problems` beside the face;
//! - a lost `=pk@<rev>` header or a revision whose baseline is no longer kept refuses, keeps the
//!   person's bytes as `<leaf>.rejected.<ext>` and writes the face again from the snapshot.
//!
//! A commit queues every write its face asks for or none of them. Once they are queued the face
//! is the snapshot's again: the pass that takes up a signed record writes the new snapshot's
//! face, and where nothing is signed yet the face is written from the snapshot as it is. What
//! this Mac queued shows in a read (`aterm file cat`) and in `outbox`, so a later save never
//! reads a committed cell, a queued new row or a held write's cell as a change again.

use super::write::{self, Env, Refusal, Said, Target, Wrote, problem};
use crate::{
    att,
    typed::{
        Collection,
        grid::{self, Absent, Base, Seen},
    },
};
use aterm_file::{Draft, Face, File, Name, OwnerAct, Snapshot, exit, snapshot::PK};
use aterm_json::{Map, Value};
use aterm_secrets::client::Paths;
use std::{cell::Cell, collections::BTreeMap, path::Path};

/// The format word of a stored baseline.
pub const FORMAT: &str = "aterm-file-base/1";

/// How long a commit waits for another commit of the same face: past one publisher bound and
/// the pass after it.
const COMMIT_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// What one revision's face showed, and what each cell's register held then.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Baseline {
    /// The grid's baseline of the projection: every stored cell's text and candidates.
    pub(super) grid: Base,
    /// The face's header cells but the key's, in face order.
    pub(super) header: Vec<String>,
    /// Each row's non-empty face cells by column name, the computed ones included.
    pub(super) cells: BTreeMap<String, BTreeMap<String, String>>,
}

impl Baseline {
    /// The baseline of `snapshot`, projected with the grid baseline `grid`: the bytes a pass
    /// keeps beside the snapshot under its revision.
    pub(super) fn encode(grid: &Base, snapshot: &Snapshot) -> Option<Vec<u8>> {
        let header: Vec<Value> = snapshot
            .columns
            .iter()
            .filter(|column| column.name != PK)
            .map(|column| Value::from(column.header(0)))
            .collect();
        let rows: Map = snapshot
            .rows
            .iter()
            .map(|row| {
                let cells: Map = row
                    .cells
                    .iter()
                    .map(|(name, text)| (name.clone(), Value::from(text.as_str())))
                    .collect();
                (row.pk.clone(), Value::Object(cells))
            })
            .collect();
        let mut object = Map::new();
        object.insert("format".to_owned(), Value::from(FORMAT));
        object.insert("grid".to_owned(), grid.to_json());
        object.insert("header".to_owned(), Value::Array(header));
        object.insert("cells".to_owned(), Value::Object(rows));
        aterm_json::to_vec(&Value::Object(object)).ok()
    }

    /// Read a stored baseline; `None` for bytes this build does not read.
    pub(super) fn decode(bytes: &[u8]) -> Option<Self> {
        let value: Value = aterm_json::from_slice(bytes).ok()?;
        if value.get("format")?.as_str()? != FORMAT {
            return None;
        }
        let grid = Base::from_json(value.get("grid")?).ok()?;
        let header = value
            .get("header")?
            .as_array()?
            .iter()
            .map(|cell| cell.as_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()?;
        let cells = value
            .get("cells")?
            .as_object()?
            .iter()
            .map(|(pk, row)| {
                let row = row
                    .as_object()?
                    .iter()
                    .map(|(name, text)| Some((name.clone(), text.as_str()?.to_owned())))
                    .collect::<Option<BTreeMap<_, _>>>()?;
                Some((pk.clone(), row))
            })
            .collect::<Option<BTreeMap<_, _>>>()?;
        Some(Self {
            grid,
            header,
            cells,
        })
    }

    /// The grid baseline a face of revision `rev` is read against: the face's own header, the
    /// key's cell `=pk@<rev>`, and each cell's text as that face showed it (a sealed value a
    /// newer build wrote shows empty, a computed cell its computed text) beside the candidates
    /// its register held.
    fn for_face(&self, rev: u64) -> Base {
        let mut header = self.header.clone();
        header.push(format!("{}{PK}@{rev}", grid::READ_ONLY));
        let none = BTreeMap::new();
        let rows = self
            .grid
            .rows
            .iter()
            .map(|(pk, seen)| {
                let shown = self.cells.get(pk).unwrap_or(&none);
                let row: BTreeMap<String, Seen> = self
                    .header
                    .iter()
                    .map(|cell| {
                        let name = cell.trim_start_matches(grid::READ_ONLY);
                        let mut held = seen.get(name).cloned().unwrap_or_default();
                        held.shown = shown.get(name).cloned().unwrap_or_default();
                        (name.to_owned(), held)
                    })
                    .collect();
                (pk.clone(), row)
            })
            .collect();
        Base {
            table: self.grid.table.clone(),
            collection: self.grid.collection.clone(),
            version: self.grid.version,
            header,
            rows,
        }
    }
}

/// The revision a face's text names in its last header cell, `=pk@<rev>`; `None` where the
/// header lost it.
pub(super) fn revision(text: &str) -> Option<u64> {
    let sheet = amacs::table::Table::scan(text, amacs::table::Delimiter::Tab).ok()?;
    let header = sheet.records.first()?;
    let last = (0..header.cells.len())
        .rev()
        .map(|at| sheet.cell(0, at).map(|cell| cell.value(text)))
        .find(|cell| cell.as_deref().is_some_and(|cell| !cell.trim().is_empty()))??;
    let rev = last
        .strip_prefix(grid::READ_ONLY)?
        .strip_prefix(PK)?
        .strip_prefix('@')?;
    (!rev.is_empty() && rev.bytes().all(|b| b.is_ascii_digit()))
        .then(|| rev.parse().ok())
        .flatten()
}

/// What a face asks for, read against its baseline.
#[derive(Debug)]
pub(super) enum Planned {
    /// One draft per row whose cells changed or that is new.
    Drafts(Vec<Draft>),
    /// Bad values: each `line:column: why`, never a value.
    Problems(Vec<String>),
    /// The face cannot be read against any baseline: why.
    Refused(String),
}

/// Where a problem is: `line:column: <column name>: why`, the line 1-based in the face's text
/// and the column 1-based.
fn problem_line(base: &Base, found: &grid::Problem) -> String {
    let column = base
        .header
        .get(found.column)
        .map_or("past the last column", |cell| {
            cell.trim_start_matches(grid::READ_ONLY)
        });
    format!(
        "{}:{}: {column}: {}",
        found.line,
        found.column + 1,
        found.rule
    )
}

/// The keys a commit's new rows are read under before the publisher derives each one's key
/// under the writer: `new-1`, `new-2`, … in the face's order, never a key the table holds.
const STAND_IN: &str = "new-";

/// Read `text`, the face of `target`, against the baseline of the revision its header names:
/// the drafts it asks for, or why it asks for nothing.
pub(super) fn plan(target: &Target, text: &str) -> Planned {
    let name = &target.name;
    let Some(rev) = revision(text) else {
        return Planned::Refused(format!(
            "the face's header lost its last cell, `={PK}@<rev>`: nothing says which revision \
             it was written at"
        ));
    };
    let Some(baseline) = target
        .held
        .baseline(rev)
        .and_then(|bytes| Baseline::decode(&bytes))
    else {
        return Planned::Refused(format!(
            "the face was written at revision {rev}, whose baseline is no longer kept here"
        ));
    };
    let compiled = match att::find(&baseline.grid.table) {
        Ok(compiled) => compiled,
        Err(_) => {
            return Planned::Refused(format!(
                "revision {rev} was projected from a table this build does not compile"
            ));
        }
    };
    let collection: &'static Collection = match compiled.collection(&baseline.grid.collection) {
        Ok(collection) if collection.name == target.collection.name => collection,
        _ => {
            return Planned::Refused(format!(
                "revision {rev} was projected from another collection than {name}'s"
            ));
        }
    };
    if baseline.grid.version != collection.version {
        return Planned::Refused(format!(
            "revision {rev} was projected at {name}'s collection version {}, and this build \
             reads version {}",
            baseline.grid.version, collection.version
        ));
    }
    let base = baseline.for_face(rev);
    let born = Cell::new(0_usize);
    let birth = |_: &Collection, _: &BTreeMap<String, Value>| {
        born.set(born.get() + 1);
        Ok(format!("{STAND_IN}{}", born.get()))
    };
    let planned = match grid::plan_saved(
        compiled.table,
        collection,
        &base,
        text,
        &birth,
        Absent::Keeps,
    ) {
        Ok(planned) => planned,
        Err(problems) => {
            return Planned::Problems(
                problems
                    .iter()
                    .map(|found| problem_line(&base, found))
                    .collect(),
            );
        }
    };
    // What a read shows now, this Mac's queued writes over the snapshot: a cell that already
    // says the face's text asks for nothing (a commit cut short and run again, a `set` the face
    // repeats).
    let read = target.held.read();
    let now = |pk: &str, column: &str| {
        read.rows()
            .iter()
            .find(|row| row.pk == pk)
            .map(|row| row.cell(column).to_owned())
    };
    let outbox = target.held.outbox();
    // A held write of this Mac's is shown `⚠ mine ‖ theirs`, never as its text: a cell a held
    // write already asks for asks for nothing again (the held write is resolved, not repeated).
    let held = |pk: &str, column: &str, text: &str| {
        outbox.held.iter().any(|held| {
            held.intent
                .edits
                .iter()
                .any(|edit| edit.key == pk && edit.column == column && edit.text == text)
        })
    };
    // A new row asks for nothing where this Mac already asked for it: a born write still queued
    // or held with the same cells, or a row the face's revision did not hold that now shows each
    // of them (a stale copy of the face saved again after its commit).
    let cells_of = |edits: &[grid::Edit]| -> BTreeMap<String, String> {
        edits
            .iter()
            .map(|edit| (edit.column.clone(), edit.mine.clone()))
            .collect()
    };
    let asked = |cells: &BTreeMap<String, String>| {
        let intents = outbox
            .waiting()
            .chain(outbox.held.iter().map(|held| &held.intent));
        intents.filter(|intent| intent.born).any(|intent| {
            let theirs: BTreeMap<&str, &str> = intent
                .edits
                .iter()
                .map(|edit| (edit.column.as_str(), edit.text.as_str()))
                .collect();
            theirs.len() == cells.len()
                && cells
                    .iter()
                    .all(|(column, text)| theirs.get(column.as_str()) == Some(&text.as_str()))
        }) || read.rows().iter().any(|row| {
            !baseline.cells.contains_key(&row.pk)
                && cells
                    .iter()
                    .all(|(column, text)| row.cell(column) == text.as_str())
        })
    };
    let mut drafts = Vec::new();
    for (pk, edits) in planned.rows {
        let new = planned.born.contains(&pk);
        if new && asked(&cells_of(&edits)) {
            continue;
        }
        let mut kept = Vec::with_capacity(edits.len());
        for edit in edits {
            if !new
                && (now(&pk, &edit.column).as_deref() == Some(edit.mine.as_str())
                    || held(&pk, &edit.column, &edit.mine))
            {
                continue;
            }
            let Some(column) = collection.column(&edit.column) else {
                continue;
            };
            kept.push(write::kept(column, edit, new));
        }
        if kept.is_empty() {
            continue;
        }
        if !new {
            write::follow_at(&target.held, &outbox, rev, &mut kept);
        }
        drafts.push(Draft {
            born: new,
            revert: None,
            edits: kept,
        });
    }
    Planned::Drafts(drafts)
}

/// Refuse the face of `held`: keep `bytes` as `<leaf>.rejected.<ext>` and write the face again
/// from the snapshot. The words to answer with.
fn refuse(held: &File, name: &Name, why: &str, bytes: &[u8]) -> Refusal {
    // The problems an earlier commit named are of bytes no longer there.
    let _ = held.put_problems("");
    let (kept, restored) = match held.restore() {
        Ok(aterm_file::Restored::Rewritten { kept: Some(kept) }) => (Ok(kept), true),
        Ok(aterm_file::Restored::Rewritten { kept: None } | aterm_file::Restored::Already) => {
            (held.keep_rejected(bytes), true)
        }
        Ok(aterm_file::Restored::Nothing) | Err(_) => (held.keep_rejected(bytes), false),
    };
    match (kept, restored) {
        (Ok(kept), true) => problem(format!(
            "{name}: {why}; nothing was queued. Your face is kept at {} and the face was written \
             again from the snapshot: make the edits there and commit again",
            kept.display()
        )),
        (Ok(kept), false) => problem(format!(
            "{name}: {why}; nothing was queued. Your face is kept at {}; the face could not be \
             written again from the snapshot, so it is left as it is (`aterm file refresh {name}`, \
             then `aterm file restore {name}`)",
            kept.display()
        )),
        (Err(error), _) => problem(format!(
            "{name}: {why}; nothing was queued, and the face is left as it is (it could not be \
             kept aside: {error})"
        )),
    }
}

/// `commit <name>`: read the face of `target` against its baseline and queue what it asks
/// for, each row's writes one intent, then give the publisher its bound. The text to print and
/// the exit code: 0, or 4 where a write was held.
///
/// # Errors
/// Nothing was queued: there is no face, it cannot be read, its header or baseline is gone
/// (its bytes kept aside), or a cell holds a bad value (`<leaf>.problems` says where).
pub(super) fn commit(
    paths: &Paths,
    config: &Path,
    target: &Target,
    act: &OwnerAct,
    env: &Env<'_>,
) -> Result<Wrote, Refusal> {
    let (held, name) = (&target.held, &target.name);
    // One commit of a face at a time: a second one reads the face once the first has queued
    // what it asks for, and so asks for nothing again.
    held.make_dirs().map_err(problem)?;
    let _commit = aterm_uds::file_lock::LockFile::take(
        &held.root().commit_lock(name),
        std::time::Instant::now() + COMMIT_WAIT,
        "another commit of this face is running",
    )
    .map_err(|error| problem(format!("{name}: {error}")))?;
    let bytes = match held.face_bytes() {
        Ok(Some(bytes)) => bytes,
        Ok(None) => {
            return Err(problem(format!(
                "{name}: no face here; `aterm file restore {name}` writes one"
            )));
        }
        Err(why) => return Err(problem(format!("{name}: the face cannot be read: {why}"))),
    };
    let clear = || held.put_problems("").map_err(problem);
    if held.face() == Face::Written {
        clear()?;
        return Ok(write::answer(name, Said::Unchanged, None, env.json));
    }
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return Err(refuse(held, name, "the face is not UTF-8", &bytes));
    };
    let drafts = match plan(target, text) {
        Planned::Refused(why) => return Err(refuse(held, name, &why, &bytes)),
        Planned::Problems(lines) => {
            let mut listed = lines.join("\n");
            listed.push('\n');
            let at = held.put_problems(&listed).map_err(problem)?;
            return Err(problem(format!(
                "{name}: the face has {} problem{}; nothing was queued (also in {}):\n{}",
                lines.len(),
                if lines.len() == 1 { "" } else { "s" },
                at.display(),
                listed.trim_end()
            )));
        }
        Planned::Drafts(drafts) => drafts,
    };
    clear()?;
    let mut ids = Vec::with_capacity(drafts.len());
    for draft in drafts {
        match held.write(act, draft, env.now_ms) {
            Ok(accepted) => ids.push((accepted.id.clone(), accepted.unflushed.clone())),
            Err(why) => return Err(unqueue(held, name, act, &ids, &why.to_string())),
        }
    }
    // Every write the face asks for is queued: the face is the snapshot's again. Its bytes are
    // taken as the face's own first, so the pass the publisher's attempt runs writes the new
    // snapshot's face over them, and where nothing is signed yet the face is written again from
    // the snapshot here. A later save of the face so never reads a committed cell, a new row
    // already queued or a held write's cell as a change again: what this Mac queued shows in a
    // read (`cat`) and in `outbox`, never as the face's own text.
    held.adopt(&bytes).map_err(problem)?;
    let id_list: Vec<String> = ids.iter().map(|(id, _)| id.clone()).collect();
    let said = if id_list.is_empty() {
        Vec::new()
    } else {
        write::attempt_all(paths, config, target, &id_list, env)
    };
    held.restore().map_err(|error| {
        problem(format!(
            "{name}: the writes are queued, but the face could not be written again from the \
             snapshot: {error}"
        ))
    })?;
    if said.is_empty() {
        // Every changed cell already says what this Mac's queued writes say.
        return Ok(write::answer(name, Said::Unchanged, None, env.json));
    }
    let mut out = String::new();
    let mut code = exit::OK;
    for (said, (_, unflushed)) in said.into_iter().zip(&ids) {
        let (text, said_code) = write::answer(name, said, unflushed.as_deref(), env.json).printed();
        out.push_str(&text);
        code = code.max(said_code);
    }
    Ok(Wrote::of_face(out, code))
}

/// A commit whose outbox refused one of its writes (`why`) queues none of them: the ones
/// already queued (`ids`) are dropped again before any is signed, and the face stays as the
/// person saved it, for a commit once the outbox has room. The words to answer with.
fn unqueue(
    held: &File,
    name: &Name,
    act: &OwnerAct,
    ids: &[(String, Option<String>)],
    why: &str,
) -> Refusal {
    let mut signed = Vec::new();
    for (id, _) in ids {
        match held.drop_intent(act, id) {
            Ok(aterm_file::Dropped::Dropped | aterm_file::Dropped::Absent) => {}
            // The publisher took it first: it stands, and a commit run again finds its cells
            // already said.
            Ok(aterm_file::Dropped::Published) | Err(_) => signed.push(id.as_str()),
        }
    }
    if signed.is_empty() {
        problem(format!(
            "{name}: the outbox refused the face's writes ({why}); nothing was queued, and the \
             face is left as it is: commit again once the outbox has room"
        ))
    } else {
        problem(format!(
            "{name}: the outbox refused the face's writes ({why}); the publisher had already \
             taken {} of them ({}), the rest were not queued, and the face is left as it is: \
             commit again once the outbox has room",
            signed.len(),
            signed.join(", ")
        ))
    }
}
