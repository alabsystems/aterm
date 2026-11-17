// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! SHARED FILES, aterm-db's side (docs/DESIGN-shared-files-2026-11-11.md): which files this
//! build compiles and the collections they project ([`types`]), the materializer that writes
//! their snapshots and faces from this Mac's replica ([`materialize`]): the registry's owner
//! files, the journal ring's Mac files, and the views folded from them ([`limits`]); the
//! publisher that
//! signs the writes this Mac queued ([`publish`]), the write verbs that queue them (`write`,
//! private to this module, and clippy.toml lets only [`command::main_entry`] and
//! [`rename`], a rename of this Mac asked for by name, run them, so an owner act is made
//! only by a verb a person or a program ran by name), and `aterm file` ([`command`]).
//!
//! Reading a shared file is `aterm_file::File::read`, in a crate that cannot reach this one: it
//! answers from the snapshot whatever this Mac's custody is doing. Only the materializer here
//! opens custody, and it turns every refusal into a state word rather than an error.

pub mod command;
pub(crate) mod growth;
pub mod limits;
pub mod materialize;
pub mod publish;
pub mod types;
mod write;

pub use types::{
    ACCOUNTS, Class, Computed, EVENTS, FILES, Fold, LIMITS, LINKS, LOGINS, MACHINES, Store, Type,
    USAGE, find,
};

use crate::typed::keys;
use aterm_secrets::client::Paths;
use std::path::Path;

pub use write::Env;

/// This Mac's own `lab/machines` row: the live row the last snapshot here marks `here` (the
/// machine this Mac's own facts row binds), with this Mac's queued writes laid over it
/// (`aterm_file::File::read`), read from the local files alone, with no custody opened. `None`
/// where no snapshot is here or it marks no row.
fn here_row(paths: &Paths) -> Option<aterm_file::Row> {
    let read =
        aterm_file::File::at(&root(&crate::generation::original(paths)), &MACHINES.file()).read();
    read.rows()
        .iter()
        .find(|row| row.live && row.cell("here") == "yes")
        .cloned()
}

/// This Mac's own `lab/machines` row, by key ([`here_row`]), as [`rename`]'s write is read
/// against it.
#[must_use]
pub fn here(paths: &Paths) -> Option<String> {
    here_row(paths).map(|row| row.pk)
}

/// This Mac's own `lab/machines` row as the last pass published it, with the cells a write of
/// this Mac's would still change ([`HereRow::pending`]: one queued, published and not yet in the
/// snapshot, or held back; `aterm_file::File::published_pending`) and its held writes of the
/// row's name cells alone that give way now ([`HereRow::held`]), read from the local files
/// alone. `None` where no snapshot is here or it marks no row.
#[must_use]
pub fn here_published(paths: &Paths) -> Option<HereRow> {
    let file = aterm_file::File::at(&root(&crate::generation::original(paths)), &MACHINES.file());
    let (published, cells) = file.published_pending();
    let row = published
        .rows()
        .iter()
        .find(|row| row.live && row.cell("here") == "yes")
        .cloned()?;
    let cell_of = |column: &str| {
        [Renamed::Nickname, Renamed::Label]
            .into_iter()
            .find(|cell| cell.column() == column)
    };
    let pending: Vec<Renamed> = [Renamed::Nickname, Renamed::Label]
        .into_iter()
        .filter(|cell| cells.contains(&(row.pk.clone(), cell.column().to_owned())))
        .collect();
    let held = if pending.is_empty() {
        Vec::new()
    } else {
        let outbox = file.outbox();
        let rev = published.snapshot.as_ref().map(|snapshot| snapshot.rev);
        outbox
            .held
            .iter()
            .filter_map(|held| {
                let edits = &held.intent.edits;
                // A write of this row alone that writes a name cell: a rename's, or an owner's
                // that carries other cells of the row with it, which gives way whole.
                if !edits.iter().all(|edit| edit.key == row.pk)
                    || !edits
                        .iter()
                        .any(|edit| edit.elem.is_none() && cell_of(&edit.column).is_some())
                {
                    return None;
                }
                let cells = edits
                    .iter()
                    .map(|edit| {
                        let column = match &edit.elem {
                            Some(elem) => format!("{}.{elem}", edit.column),
                            None => edit.column.clone(),
                        };
                        (column, edit.text.clone())
                    })
                    .collect();
                // A register moved under the write: the row read here shows the value that moved
                // it only once its baseline holds other candidates than the write was read
                // against. The write gives way once the row shows every value it met (each cell
                // the hold names; every cell where it names none of them), and stays pending
                // until then.
                let shows = |edit: &aterm_file::Edit| {
                    rev.and_then(|rev| write::candidates_at(&file, rev, edit))
                        .is_some_and(|now| against(&outbox, edit).is_none_or(|was| was != now))
                };
                let shown = publish::moved(&held.why).is_none_or(|moved| {
                    let named = |edit: &&aterm_file::Edit| {
                        let cell = format!("{}.{}", keys::safe_key(&edit.key), edit.column);
                        moved.contains(&cell.as_str())
                    };
                    let met: Vec<&aterm_file::Edit> = edits.iter().filter(named).collect();
                    if met.is_empty() {
                        edits.iter().all(shows)
                    } else {
                        met.into_iter().all(shows)
                    }
                });
                shown.then(|| HeldName {
                    id: held.intent.id.clone(),
                    cells,
                })
            })
            .collect()
    };
    Some(HereRow {
        row,
        live: published.status.word == aterm_file::Word::Live,
        pending,
        held,
    })
}

/// The candidates `edit`, a held write's edit of one cell, was written against, lower-case hex in
/// order: its own expected operations, or what the earlier intent of this Mac it follows
/// (`intent:<id>`) left in the edit's own register, as the publisher reads it (its operation
/// where the marker says nothing of that register). `None` where that intent's marker is no
/// longer kept.
fn against(outbox: &aterm_file::Outbox, edit: &aterm_file::Edit) -> Option<Vec<String>> {
    let mut was = match edit.expected.as_slice() {
        [entry] if entry.starts_with(aterm_file::outbox::INTENT) => {
            let id = entry.strip_prefix(aterm_file::outbox::INTENT)?;
            let done = outbox.done.iter().find(|done| done.intent.id == id)?;
            let register = crate::typed::cell::CellKey {
                pk: &edit.key,
                col: &edit.column,
                elem: edit.elem.as_deref(),
            }
            .encode()
            .ok()?;
            match done.left.get(&register) {
                Some(left) => left.clone(),
                None => done.op.clone().into_iter().collect(),
            }
        }
        expected => expected.to_vec(),
    };
    was.sort_unstable();
    Some(was)
}

/// This Mac's own `lab/machines` row as [`here_published`] reads it.
#[derive(Clone, Debug)]
pub struct HereRow {
    row: aterm_file::Row,
    live: bool,
    pending: Vec<Renamed>,
    held: Vec<HeldName>,
}

/// One held write of this Mac's own `lab/machines` row that writes a name cell and gives way
/// now ([`HereRow::held`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeldName {
    /// The intent's id, which [`yield_held`] drops.
    pub id: String,
    /// Each cell it writes (its column, `<column>.<element>` for a set's element), with the
    /// value it asked for.
    pub cells: Vec<(String, String)>,
}

impl HereRow {
    /// The row's key.
    #[must_use]
    pub fn pk(&self) -> &str {
        &self.row.pk
    }

    /// Whether the snapshot the row was read from is live: the status beside it vouches for it
    /// and calls it `live`. A retained one (held, off, of another revision) is display data.
    #[must_use]
    pub const fn live(&self) -> bool {
        self.live
    }

    /// The nickname the published row holds.
    #[must_use]
    pub fn nickname(&self) -> crate::alab::view::MachineNickname {
        crate::alab::view::MachineNickname::of_file_row(&self.row)
    }

    /// The label the published row holds.
    #[must_use]
    pub fn label(&self) -> crate::alab::view::MachineLabel {
        crate::alab::view::MachineLabel::of_file_row(&self.row)
    }

    /// Whether a write of `cell` this Mac queued, published and the snapshot does not hold yet,
    /// or one the publisher held, would change what the published row shows. The naming read
    /// drops a held write of the row's name cells ([`yield_held`]) once the row shows what it
    /// met.
    #[must_use]
    pub fn pending(&self, cell: Renamed) -> bool {
        self.pending.contains(&cell)
    }

    /// This Mac's held writes of this row alone that write a name cell (its nickname, its
    /// number, or both as one rename wrote them, with any other cell of the row an owner's
    /// `aterm file set` carried beside them) that give way now, whole ([`yield_held`]): one that
    /// met another writer's value, once this row shows every value it met (its baseline holds
    /// other candidates than the write was read against), and one held for any other reason,
    /// which no later read changes. A write held against a value the snapshot does not show yet
    /// stays [`Self::pending`] until a pass brings it; no name cell waits on a person.
    #[must_use]
    pub fn held(&self) -> &[HeldName] {
        &self.held
    }
}

/// The cell of this Mac's `lab/machines` row a rename writes ([`rename`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Renamed {
    /// `nickname`.
    Nickname,
    /// `label`, the machine's number.
    Label,
}

impl Renamed {
    /// The column's name.
    #[must_use]
    pub const fn column(self) -> &'static str {
        match self {
            Self::Nickname => "nickname",
            Self::Label => "label",
        }
    }
}

/// A vouched live snapshot's own row and nickname, including queued local edits.
/// This is evidence only: an automatic local write must also admit the current native
/// registry source through its persistence. Retained held or off rows remain display data.
#[must_use]
pub fn live_here_nickname(paths: &Paths) -> Option<(String, crate::alab::view::MachineNickname)> {
    let read =
        aterm_file::File::at(&root(&crate::generation::original(paths)), &MACHINES.file()).read();
    if read.status.word != aterm_file::Word::Live {
        return None;
    }
    let mut rows = read
        .rows()
        .iter()
        .filter(|row| row.live && row.cell("here") == "yes");
    let row = rows.next()?;
    if rows.next().is_some() {
        return None;
    }
    Some((
        row.pk.clone(),
        crate::alab::view::MachineNickname::of_file_row(row),
    ))
}

/// A rename of this Mac that a person or a program asked for by name (`aterm name
/// nick|set|undo`, Rename This Mac…, Settings ▸ This Mac, Undo Last Rename): the `lab/machines`
/// row `row` (its key, [`here`]) gets each of `cells`, the nickname (tracker package FL-144) and
/// the number (FL-170), as one write (FL-194), queued and offered to the publisher for
/// `env.attempt` (none: queued only, for [`offer`] or a later pass) exactly as `aterm file set
/// lab/machines <row> nickname=<word> label=<number>` queues it, so the registry keeps who wrote
/// it, when, and every earlier value, and one operation holds the rename. Cells the row says
/// already are left out. clippy.toml lets only those verbs run it, as it lets only `aterm
/// file`'s entry run the write verbs: each makes an owner act. Returns the id of the intent it
/// queued (published or held, for a nonzero attempt), or `None` where the row says every cell
/// already.
///
/// # Errors
/// Nothing was queued, with the reason: as `aterm file set` refuses (no snapshot of
/// `lab/machines` here, no row `row`, one not published yet, a value a column refuses, no cell,
/// an owner act `env.by` cannot name).
pub fn rename(
    paths: &Paths,
    config: &Path,
    row: &str,
    cells: &[(Renamed, &str)],
    env: &Env<'_>,
) -> Result<Option<Queued>, String> {
    let mut words = vec!["set".to_owned(), MACHINES.name.to_owned(), row.to_owned()];
    words.extend(
        cells
            .iter()
            .map(|(cell, value)| format!("{}={value}", cell.column())),
    );
    #[expect(
        clippy::disallowed_methods,
        reason = "`rename`, the one door a rename of this Mac asked for by name takes: it runs \
                  the explicit `aterm file set` verb on this Mac's own row"
    )]
    let said = write::verb(paths, config, &words, env);
    said.map(|wrote| {
        wrote.id().map(|id| Queued {
            id: id.to_owned(),
            unflushed: wrote.unflushed().map(str::to_owned),
        })
    })
    .map_err(|refused| match refused {
        write::Refusal::Usage(why) | write::Refusal::Problem(why) => why,
    })
}

/// A rename's write that [`rename`] queued: the intent's id, and why its outbox folder's flush
/// failed where it did (the outbox holds it, but a power loss before it is published may lose
/// it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Queued {
    /// The intent's id.
    pub id: String,
    /// Why the outbox folder's flush failed, where it did.
    pub unflushed: Option<String>,
}

/// Why this Mac's `lab/machines` row `row` would refuse `value` as its `cell`, as `aterm file
/// set` would refuse it (the column's text rule and the value guard), or `None` where it takes
/// it: what a rename's dry run checks, queueing nothing.
#[must_use]
pub fn cell_refusal(row: &str, cell: Renamed, value: &str) -> Option<String> {
    let collection = &crate::alab::registry::MACHINE;
    let column = collection.column(cell.column())?;
    crate::typed::grid::cell_edits(collection, row, column, value, None).err()
}

/// Give way with this Mac's held write `id` of its own `lab/machines` row's name cell
/// ([`HereRow::held`]): the write met another writer's value of the cell, the registry's value
/// stands and the name follows it, so the held write is dropped exactly as `aterm file resolve
/// lab/machines <id> theirs` drops it. The naming code decides this and journals it; no person
/// is asked (the owner, 2026-11-16, `docs/OWNER_RULINGS.md`). clippy.toml lets only aterm-agent's
/// `machine_name::registry` run it, as it lets only that module run [`rename`].
///
/// # Errors
/// Nothing was dropped, with the reason: no such held write is here now.
pub fn yield_held(paths: &Paths, config: &Path, id: &str, env: &Env<'_>) -> Result<(), String> {
    let words = [
        "resolve".to_owned(),
        MACHINES.name.to_owned(),
        id.to_owned(),
        "theirs".to_owned(),
    ];
    #[expect(
        clippy::disallowed_methods,
        reason = "`yield_held`, the one door the naming code gives way through: it runs the \
                  explicit `aterm file resolve … theirs` verb on this Mac's own held name write"
    )]
    let said = write::verb(paths, config, &words, env);
    said.map(drop).map_err(|refused| match refused {
        write::Refusal::Usage(why) | write::Refusal::Problem(why) => why,
    })
}

/// Offer what this Mac queued for `lab/machines` to the publisher for `attempt`, as a write's own
/// attempt does, projecting the file again where anything was signed, and say what became of the
/// write `queued` in `aterm file set`'s words (`published <operation>`, `queued <id>: <why>` or
/// `held <id>: <why>`, with its flush note): a rename that queued its cells with no attempt
/// while it held the naming lock ([`rename`] with a zero [`Env::attempt`]) is published by the
/// command that made it. What stays queued is published by a later pass.
#[must_use]
pub fn offer(
    paths: &Paths,
    config: &Path,
    queued: &Queued,
    attempt: std::time::Duration,
    now_ms: u64,
) -> String {
    write::offered(
        paths,
        config,
        &MACHINES.file(),
        (&queued.id, queued.unflushed.as_deref()),
        attempt,
        now_ms,
    )
}

/// Where this Mac's shared files live: `~/.aterm/files` of the home `paths` names. A test's
/// stand-in home so keeps its files in its own scratch folder.
#[must_use]
pub fn root(paths: &Paths) -> aterm_file::Root {
    aterm_file::Root::for_home(&paths.home)
}

/// Seams another crate's tests take a stand-in lab's home through: a writer succession its
/// strict readers refuse while a shared file still answers. Absent from release builds.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub mod fixture {
    use aterm_secrets::{client::Paths, commons::bootstrap::Bootstrap};

    /// Fence `paths` under a writer succession whose candidate is never admitted, so it stays
    /// pending (FL-14, ATERM-KI-1109-1): every strict reader of the home refuses with "Commons
    /// writer succession is still preserving old records", and a materializer pass records its
    /// records `held` (`succession`).
    pub fn fence_pending(paths: &Paths, bootstrap: &Bootstrap) {
        crate::generation::file_seams::fence_pending(paths, bootstrap);
    }
}

/// Seams of this crate's tests: how often this thread reached for custody, a step run between
/// a pass's first look at its home's standing and its open of custody, one run between a pass's
/// read and its record, one run where a table view is about to authenticate the records a roll
/// carried, a part a pass's key leaves out, a later build's rules, the roster a pass reads (this
/// Mac's machine, a member that left, a roster that does not read), and the owner act and `set`
/// draft a test outside this module writes with.
#[cfg(test)]
#[path = "file_seam_tests.rs"]
pub(crate) mod seam;

#[cfg(test)]
mod model_read_tests;
#[cfg(test)]
pub(crate) mod model_tests;
#[cfg(test)]
mod tests;
