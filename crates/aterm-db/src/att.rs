// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! ATT/1 tables on this Mac's Commons (docs/DESIGN-alab-lab-model-2026-11-01.md §3, §8): the
//! store side of [`crate::typed`] and the generic `aterm db table` verbs ([`command`]).
//!
//! - [`View`]: one read of a typed table, the per-Mac quota applied: past-quota automatic
//!   records never project, owned rows count only at their signer's own key.
//! - [`Handle`]: read (at the head or as of a past point, [`Point`]), compare two points
//!   ([`Handle::diff`]), apply a grid save with compare-before-write, revert one operation,
//!   read history in pages ([`history`]), and list every value one cell held and what one write
//!   replaced and had not seen ([`Handle::cell_values`], [`Handle::wrote`]), for any compiled
//!   table; append to a log at keys per writer ([`Handle::append_log`]), and read it typed
//!   ([`View::logs`]).
//! - [`command`]: `aterm db table ls|schema|get|scan|history|diff|check|revert|export|apply|edit`,
//!   and the amacs grid round trip. A secret cell shows as the name of its secret-store entry
//!   ([`typed::secret`]); nothing here holds a secret's value.

#[cfg(unix)]
pub(crate) mod checkpoint;
#[cfg(unix)]
pub use checkpoint::ledger_mark;
pub mod command;
mod diff;
mod edit;
pub mod history;
pub mod kept;
pub mod roll;
mod values;

use crate::said::Said;
use crate::{
    Error, Result,
    alab::registry,
    catalog::Descriptor,
    database::{Change, Command, MAX_WRITES},
    documents::{application, log::Log},
    lab::tracker,
    table::Session,
    typed::{
        self, Collection, Table,
        cell::{self, Cells},
        grid::{self, Plan, Stale},
        owned::{self, Op, SchemaState},
        state::State,
    },
};
use astream_wire::sync::{Actor, Digest};
use aterm_json::Value;
use aterm_secrets::{client::Paths, commons::bootstrap::Bootstrap};
use std::collections::{BTreeMap, BTreeSet};

pub use crate::documents::log::Hlc;
pub use diff::{CellDiff, Delta, Diff, Maker, Point};
pub(crate) use edit::edit as edit_grid;
pub use edit::{Edited, Opener, amacs, open_every_ordinary, open_ordinary};
pub use history::{
    Ask, Cursor, Entry, EntryKind, Filter, HistoryView, Inert, Kept, Mac, Macs, Page, Revision,
    Writer,
};
pub use values::{Held, Source, Written, Wrote};

/// How a table keys the row a new grid row becomes, from the typed values of its cells and the
/// table's cells as they are (an account's key needs its login's address).
pub type Birth = fn(&Collection, &BTreeMap<String, Value>, &Cells) -> Result<String>;

/// A table's rules on what the owner writes, over each row's changes (one record each) and the
/// table's cells as they are under the writer: it answers the first write the rules stop
/// ([`Refused`]), and may add to a row's record the cells its rules derive from one the write
/// changes. It runs through [`Compiled::rules`] for [`Author::Owner`], before the rules every
/// table keeps on every write ([`typed::check::refused`]), on a grid save ([`Handle::apply`]),
/// an owner verb's write ([`Handle::commit_owner`]) and a revert ([`Handle::revert`], which
/// writes the exact compensation and none of the additions); a plan written through
/// [`Handle::commit_derived`] or [`Handle::commit_sealed_until`] (the importer's, `adopt`'s,
/// `prefer`'s) and an automatic pass's write pass only the rules every table keeps.
///
/// # Errors
/// A change is not a typed cell of its collection.
pub type Guard = fn(&mut [Vec<crate::database::Change>], &Cells) -> Result<Option<Refused>>;

pub use crate::typed::Refused;

/// Who authors a write [`Compiled::rules`] judges. Every write this Mac authors is one of these;
/// a peer's record arriving by sync is none and is never judged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Author {
    /// The owner's own word: a grid save, an owner verb, a revert. The table's [`Guard`] runs
    /// first.
    Owner,
    /// A plan derived from the table and a source: the importer, `adopt`, `prefer`, a seed,
    /// registration or observation pass.
    Derived,
}

/// A typed catalog table this build compiles.
#[derive(Clone, Copy, Debug)]
pub struct Compiled {
    pub table: &'static Table,
    /// What a person calls the family this generation is of (`Registry`): the heading the
    /// Tables page's rail lists its tables under. Every generation of a family says the same.
    pub title: &'static str,
    /// The table descriptor's version.
    pub version: u32,
    pub birth: Birth,
    /// Its rules on the owner's writes ([`Author::Owner`]).
    pub guard: Guard,
    /// Whether this build compiles a row keyed `pk` in `collection` (the registry's seeds): a
    /// seed pass of this build writes it wherever it is absent, so a write made on this build
    /// may name it before a pass has written it here ([`Self::rules`]). Another build may
    /// compile fewer seed rows or more.
    pub seeded: fn(&str, &str) -> bool,
    /// Which generation of its table family this is (`registry` 1, `registry.2` 2, design §3.8).
    pub generation: u32,
    /// The next generation a roll restates this one into, when this build compiles one.
    pub successor: Option<&'static Compiled>,
    /// The newest record version this generation takes ([`Self::decoder`]): 1, the record
    /// every build reads, or 2, which also says who made it, why and on what evidence
    /// ([`crate::said`]). A generation takes 2 only where every build that compiles it
    /// decodes version 2, so no build reads a generation whose records it refuses (design
    /// docs/DESIGN-lab-tracker-2026-11-14.md §7, finding 2): a version 2 generation is a new
    /// one, never one a build before version 2 compiled. Every registry generation takes 1:
    /// v0.202.0 compiles the first three and v0.203.0 all four, each with a version 1 decoder.
    pub records: u8,
    /// Whether each row an owner's write changes is a record of its own ([`Self::packed`]): a
    /// record says once who made it and why ([`crate::said`]), so rows packed into one record
    /// would share one reason (docs/DESIGN-lab-tracker-2026-11-14.md §7, finding 10). The
    /// registry's generations pack a save's rows together, as every build that reads them has.
    pub rows_apart: bool,
}

impl Compiled {
    /// This table's catalog declaration in `trust`'s lab.
    ///
    /// # Errors
    /// The table's words break the descriptor grammar.
    pub fn descriptor(&self, trust: aterm_secrets::commons::journal::Trust) -> Result<Descriptor> {
        Descriptor::application(trust, self.table.application, self.table.role, self.version)
    }

    /// How this generation's records are read: [`decode`] where it takes version 1 records
    /// only, refusing a version 2 record as every build before version 2 refuses one; else
    /// [`decode_said`].
    pub const fn decoder(&self) -> fn(&[u8]) -> Result<Command> {
        if self.records >= 2 {
            decode_said
        } else {
            decode
        }
    }

    /// The statement a record of this generation carries for `said`: all of it where the
    /// generation takes version 2 records, none where it takes version 1 only.
    pub fn carries(&self, said: Option<&Said>) -> Option<Box<Said>> {
        said.filter(|_| self.records >= 2).cloned().map(Box::new)
    }

    /// Refuse `command` where it is a version 2 record and this generation takes version 1
    /// records only: what every write into the generation passes before it is signed.
    ///
    /// # Errors
    /// The command says who made it, and this generation's readers would refuse it.
    pub fn admits(&self, command: &Command) -> Result<()> {
        if command.said().is_some() && self.records < 2 {
            return Err(Error::new(&format!(
                "{}: this generation takes version 1 records only; the builds that read it \
                 refuse a record that says who made it",
                self.name()
            )));
        }
        Ok(())
    }

    /// This generation and every later one this build compiles, oldest first.
    pub fn chain(&'static self) -> Vec<&'static Compiled> {
        std::iter::successors(Some(self), |compiled| compiled.successor).collect()
    }

    /// Its rules on a write's `rows` (one record each) that this Mac authors over `view`, the
    /// table as the writer reads it: for the owner's own word, that a fixed column is written
    /// only at its row's birth ([`typed::check::fixed_after_birth`]) and the table's own
    /// ([`Self::guard`]), which may add derived cells; then, whoever authors it, the rules every
    /// table keeps ([`typed::check::refused`]): a reference the write makes names a row the table
    /// holds once the write lands or compiles ([`Self::seeded`]), no two live rows come to share
    /// a unique set, no row live rows reference is retired, and no live row comes to name a row
    /// that is not live. Every path that appends a record this Mac authors to a typed table
    /// passes it once over the whole write, before the first record: a grid save, an owner verb,
    /// a revert, the shared files' publisher and every derived plan; a roll's restatement, which
    /// carries what the sealed generation's Macs wrote, does not. A derived write to a fixed
    /// column after birth is not refused here: the authority rules hold it inert on every read
    /// (`typed::authority`). Answers the first refusal.
    ///
    /// # Errors
    /// A change is not a typed cell of its collection.
    pub fn rules(
        &self,
        rows: &mut [Vec<Change>],
        view: &View,
        author: Author,
    ) -> Result<Option<Refused>> {
        if let Some(refused) = self.owner_rules(rows, &view.cells, author)? {
            return Ok(Some(refused));
        }
        Ok(typed::check::refused(self.table, rows, view, self.seeded))
    }

    /// The rules of [`Self::rules`] held to the owner's own word, which read only the table's
    /// `cells`: a fixed column is written only at its row's birth, and the table's own
    /// ([`Self::guard`]), which may add derived cells to a row's record.
    fn owner_rules(
        &self,
        rows: &mut [Vec<Change>],
        cells: &Cells,
        author: Author,
    ) -> Result<Option<Refused>> {
        if author != Author::Owner {
            return Ok(None);
        }
        if let Some(refused) = typed::check::fixed_after_birth(self.table, rows, cells) {
            return Ok(Some(refused));
        }
        (self.guard)(rows, cells)
    }

    /// The records an owner's write of `rows` (each row's changes) appends, in order, each
    /// carrying `said`: every row a record of its own where this generation keeps its rows
    /// apart ([`Self::rows_apart`]), else as many rows to a record as fit ([`pack_saying`]). A
    /// row is never split across two records.
    ///
    /// # Errors
    /// One row alone passes a record's limits.
    pub(crate) fn packed(
        &self,
        rows: Vec<Vec<Change>>,
        said: Option<&Said>,
    ) -> Result<Vec<Vec<Change>>> {
        if !self.rows_apart {
            return pack_saying(rows, said);
        }
        let mut records = Vec::with_capacity(rows.len());
        for row in rows.into_iter().filter(|row| !row.is_empty()) {
            records.extend(pack_saying(vec![row], said)?);
        }
        Ok(records)
    }

    /// The records a grid save of `plan` to `collection` appends where it lands on the table as
    /// `cells` hold it, in the order the save appends them: the plan settled against those cells
    /// ([`grid::settle`]), with what the owner's rules add to it ([`Self::rules`]), in the form
    /// ([`conform`]) and the records [`Self::packed`] packs it in, as [`Handle::apply`] writes it. `None` where
    /// those rules refuse it, so the save writes nothing. A page that holds a read's cells asks
    /// it, before a Write, which record carries each cell (a claim one record begins comes after
    /// one an earlier record begins); the rules every table keeps, which read the whole view,
    /// refuse a save but add nothing to it, and are the save's to judge.
    ///
    /// # Errors
    /// A change is not a typed cell of its collection, or one row passes a record's limits.
    pub fn appended(
        &self,
        collection: &Collection,
        plan: &Plan,
        cells: &Cells,
    ) -> Result<Option<Vec<Vec<Change>>>> {
        let mut rows = grid::settle(collection, plan, cells).rows;
        if self.owner_rules(&mut rows, cells, Author::Owner)?.is_some() {
            return Ok(None);
        }
        for row in &mut rows {
            conform(self.table, row)?;
        }
        self.packed(rows, None).map(Some)
    }

    /// The key a new grid row in `collection` is born under ([`Birth`]), refused when the
    /// table already holds a row there: a `--where` export can leave that row out of the
    /// file, and a birth never writes into a row the owner did not see (design §8).
    ///
    /// # Errors
    /// The table's birth rule stops the key, or the table holds that row already.
    pub fn born(
        &self,
        collection: &Collection,
        values: &BTreeMap<String, Value>,
        cells: &Cells,
    ) -> Result<String> {
        let pk = (self.birth)(collection, values, cells)?;
        if grid::holds_row(collection, cells, &pk) {
            return Err(Error::new(&format!(
                "this new row is the row {pk}, which the table already holds; export without \
                 a filter and edit that row"
            )));
        }
        Ok(pk)
    }

    /// `<application>/<role>`.
    pub fn name(&self) -> String {
        format!("{}/{}", self.table.application, self.table.role)
    }

    pub(crate) fn matches_name(&self, name: &str) -> bool {
        name.strip_prefix(self.table.application)
            .and_then(|rest| rest.strip_prefix('/'))
            .is_some_and(|role| role == self.table.role)
    }

    /// The collection named `name`: cells, owned or a log.
    ///
    /// # Errors
    /// The table has no such collection.
    pub fn collection(&self, name: &str) -> Result<&'static Collection> {
        self.table
            .collection(name)
            .ok_or_else(|| Error::new(&format!("{}: no collection of that name", self.name())))
    }
}

/// `alab/registry`, the lab model's owner data, until a roll seals it ([`REGISTRY_2`]).
pub const REGISTRY: Compiled = Compiled {
    table: &registry::REGISTRY,
    title: registry::TITLE,
    version: registry::VERSION,
    birth: registry::birth,
    guard: registry::guard,
    seeded: crate::alab::seeds::compiled,
    generation: 1,
    successor: Some(&REGISTRY_2),
    records: 1,
    rows_apart: false,
};

/// `alab/registry.2`, the registry's second generation (design §3.8, step 13).
pub const REGISTRY_2: Compiled = Compiled {
    table: &registry::REGISTRY_2,
    title: registry::TITLE,
    version: registry::VERSION,
    birth: registry::birth,
    guard: registry::guard,
    seeded: crate::alab::seeds::compiled,
    generation: 2,
    successor: Some(&REGISTRY_3),
    records: 1,
    rows_apart: false,
};

/// `alab/registry.3`, the registry's third generation: the first whose history holds no secret.
/// The roll into it moves each sealed value earlier generations hold into the secret store and
/// carries only the reference (docs/PLAN.md LT-04, `alab::roll`).
pub const REGISTRY_3: Compiled = Compiled {
    table: &registry::REGISTRY_3,
    title: registry::TITLE,
    version: registry::VERSION,
    birth: registry::birth,
    guard: registry::guard,
    seeded: crate::alab::seeds::compiled,
    generation: 3,
    successor: Some(&REGISTRY_4),
    records: 1,
    rows_apart: false,
};

/// `alab/registry.4`, the registry's fourth generation: the first whose free-text lists (a Mac's
/// nickname aliases and hosts, a mailbox's domains) are text sets. The roll into it restates
/// each whole list as its members, every list candidate's provenance kept (LT-15B,
/// `alab::roll`).
pub const REGISTRY_4: Compiled = Compiled {
    table: &registry::REGISTRY_4,
    title: registry::TITLE,
    version: registry::VERSION,
    birth: registry::birth,
    guard: registry::guard,
    seeded: crate::alab::seeds::compiled,
    generation: 4,
    successor: None,
    records: 1,
    rows_apart: false,
};

/// `lab/tracker`, the lab tracker's tables (docs/DESIGN-lab-tracker-2026-11-14.md,
/// `crate::lab::tracker`): its cells tables and its logs, a row born under the key its name
/// makes and an owner's write held to the tracker's own rules. It holds no seed. It is a new
/// generation, declared with version 2 writes (design §5, step 3), so a record says who made it
/// where its writer states so, and each row an owner's write changes is a record of its own.
/// No build registers it for production yet ([`FAMILIES`]; TK-32): only a test declares it.
pub const TRACKER: Compiled = Compiled {
    table: &tracker::TRACKER,
    title: tracker::TITLE,
    version: tracker::VERSION,
    birth: tracker::mapped::birth,
    guard: tracker::checks::guard,
    seeded: no_seed,
    generation: 1,
    successor: None,
    records: 2,
    rows_apart: true,
};

/// No row is compiled in.
fn no_seed(_: &str, _: &str) -> bool {
    false
}

/// Every typed table family this build registers, each by its first generation: what the
/// `aterm db table` verbs, checkpoints, the Tables page and its Check reach, each family's later
/// generations through its chain ([`generations`]). `lab/tracker` ([`TRACKER`]) joins at its
/// go-live (TK-32): until then it is written and read through [`Handle`] and its shared files on
/// test replicas only (decision TK-D14), so no build declares a generation of it that a later
/// one must keep.
pub const FAMILIES: &[&Compiled] = &[&REGISTRY];

/// Every generation of every family in [`FAMILIES`], each family's oldest first.
pub fn generations() -> impl Iterator<Item = &'static Compiled> {
    FAMILIES.iter().flat_map(|first| first.chain())
}

/// The families a test registered for its own replicas ([`register_for_test`]): a family no
/// build registers, which the catalog must still know as one this build compiles.
#[cfg(test)]
static TEST_FAMILIES: std::sync::Mutex<Vec<&'static Compiled>> = std::sync::Mutex::new(Vec::new());

/// Register `first`'s family for this test process, once: what [`FAMILIES`] does for a family a
/// build registers. Every test replica is a lab of its own, so a family registered by one test
/// is declared in no other test's catalog.
#[cfg(test)]
pub(crate) fn register_for_test(first: &'static Compiled) {
    let mut families = TEST_FAMILIES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !families.iter().any(|held| held.name() == first.name()) {
        families.push(first);
    }
}

/// Every family this build registers ([`FAMILIES`]), each by its first generation; in a test
/// build, the families a test registered too ([`register_for_test`]).
pub(crate) fn families() -> Vec<&'static Compiled> {
    #[cfg(test)]
    {
        let mut all = FAMILIES.to_vec();
        all.extend(
            TEST_FAMILIES
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .iter()
                .copied(),
        );
        all
    }
    #[cfg(not(test))]
    FAMILIES.to_vec()
}

/// Whether a roll of `first`'s family keeps every signed record (decisions TK-D2 and TK-D17,
/// docs/DESIGN-lab-tracker-2026-11-14.md §8): none of its generations declares a secret column
/// ([`Table::holds_secret`]). A signed record cannot be carried with a secret value taken out,
/// so a family that declares one has its history restated by the roll, within a bound and
/// behind a horizon (`alab::history`), and its sealed generation released (`alab::roll`). A
/// family that declares none loses nothing by keeping the record itself: its roll
/// ([`roll`]) restates the registers alone, the sealed generation's log stays on every Mac
/// that holds it and is repaired to every Mac that does not (`catalog::State::Kept`), and
/// history, as-of and diff read each record there under its own signer's signature
/// ([`kept`]).
pub fn keeps_records(first: &'static Compiled) -> bool {
    !first
        .chain()
        .iter()
        .any(|compiled| compiled.table.holds_secret())
}

/// The generation of `chain` (oldest first) readers and writers use in `snapshot`: the oldest
/// one subscribed here. A roll declares the next generation and restates into it while the old
/// one stays subscribed, so every Mac keeps reading the old one until the roll seals it; then
/// the old one's declarations conflict (or, in a family that keeps its records, read as
/// `catalog::State::Kept`) and the next is the oldest subscribed. With none
/// subscribed it is the first, whose read then says why.
pub(crate) fn current_in(
    snapshot: &crate::catalog::Snapshot,
    trust: aterm_secrets::commons::journal::Trust,
    chain: &[&'static Compiled],
) -> &'static Compiled {
    chain
        .iter()
        .copied()
        .find(|compiled| {
            compiled.descriptor(trust).is_ok_and(|descriptor| {
                snapshot
                    .entries
                    .get(&descriptor.scope.digest())
                    .is_some_and(|entry| entry.state == crate::catalog::State::Subscribed)
            })
        })
        .unwrap_or(chain[0])
}

/// The generation of `first`'s family this Mac's catalog says to use ([`current_in`]), read as
/// an observer: `first` itself where it has no successor, where there is no Commons, or where
/// the catalog cannot be read (the read that follows then says why).
pub fn current(paths: &Paths, first: &'static Compiled) -> &'static Compiled {
    let chain = first.chain();
    if chain.len() == 1 {
        return first;
    }
    application::peek_catalog(paths)
        .ok()
        .flatten()
        .map_or(first, |(trust, snapshot)| {
            current_in(&snapshot, trust, &chain)
        })
}

/// Whether `first`'s family has one generation that can name a store entry on this Mac, and it is
/// `at`: the current one ([`current_in`]) is `at`, and every other generation of the family in
/// this Mac's catalog, one this build compiles or one it does not (a newer build's), is neither
/// subscribed nor stored here. A roll declares the next generation and carries references into
/// it while the current one stays subscribed, and a sealed generation keeps its storage, with
/// records not yet carried, until this Mac releases it; while any such generation is there, the
/// entries the family names are not all one view shows, and nothing may be collected by that view
/// ([`crate::typed::secret::retire`], [`crate::typed::secret::sweep`]). Asked after a write to
/// `at`, a roll declared or sealed since then makes the answer `false`. `false` where the catalog
/// or its storage cannot be read.
pub fn sole_generation(paths: &Paths, first: &'static Compiled, at: &'static Compiled) -> bool {
    let chain = first.chain();
    let Ok(Some((trust, snapshot))) = application::peek_catalog(paths) else {
        return chain.len() == 1 && first.generation == at.generation;
    };
    if current_in(&snapshot, trust, &chain).generation != at.generation {
        return false;
    }
    let Ok(here) = at.descriptor(trust) else {
        return false;
    };
    let directory = crate::catalog::Directory::new(paths, trust);
    let family = |descriptor: &Descriptor| {
        descriptor.application == first.table.application
            && descriptor
                .role
                .strip_prefix(first.table.role)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
    };
    snapshot.entries.iter().all(|(digest, entry)| {
        *digest == here.scope.digest()
            || !entry
                .declarations
                .iter()
                .any(|declaration| family(&declaration.descriptor))
            || (entry.state != crate::catalog::State::Subscribed
                && directory
                    .path(entry.scope)
                    .is_ok_and(|path| path.try_exists().is_ok_and(|stored| !stored)))
    })
}

/// `changes`, about to be appended to the generation `table`, as that generation holds them:
/// each cell change of a cells collection is checked again against `table`'s own collection of
/// that name ([`cell::change`]), its schema version set to that collection's. A build writes one
/// collection definition, its newest, while a generation it writes keeps the shape the builds
/// reading it know: the registry's first two generations type a password and a phone's number
/// as a sealed token, the shape builds up to 0.200.0 read, so a secret-store reference or a
/// plain number there is refused here, whatever path wrote it, and only the third generation
/// on, which no older build reads, takes one; and the first three hold a Mac's nickname aliases
/// and hosts and a mailbox's domains as one whole list, so an element of them is refused there
/// and a whole list is refused in the fourth, which holds them as text sets.
///
/// # Errors
/// A change is no typed cell value, or `table`'s collection refuses it; the error names the
/// generation and the cell's column, never its value.
/// Whether some generation of [`FAMILIES`] types `collection.column` otherwise than `column` does:
/// a list or set column whose form a generation sets apart (a Mac's hosts, a mailbox's domains),
/// which [`conform`] re-types, while a set or list every generation holds alike is carried as
/// it is.
fn set_apart(collection: &str, column: &typed::Column) -> bool {
    generations().any(|generation| {
        generation
            .table
            .collection(collection)
            .and_then(|known| known.column(column.name))
            .is_some_and(|other| other.ty != column.ty)
    })
}

pub fn conform(table: &Table, changes: &mut [Change]) -> Result<()> {
    for change in changes {
        let Some(collection) = table.collection(&change.collection) else {
            continue;
        };
        if collection.kind != typed::Kind::Cells {
            continue;
        }
        let Some(value) = change.value.as_ref() else {
            continue;
        };
        let key = cell::CellKey::parse(&change.key)?;
        // Only a column whose type a generation sets apart (a sealed token before the third, a
        // reference or a number from it; a whole list before the fourth, a text set's elements
        // from it) is re-typed; every other column is the same in each.
        let Some(column) = collection.column(key.col) else {
            continue;
        };
        let collection_form = matches!(column.ty, typed::Ty::List(..) | typed::Ty::Set(_))
            && set_apart(&change.collection, column);
        if !collection_form
            && !matches!(
                column.ty,
                typed::Ty::Secret | typed::Ty::SecretRef | typed::Ty::Phone
            )
        {
            continue;
        }
        // One candidate as this generation types it; a roll's restatement carries a register's
        // candidates together ([`cell::carry`]), each checked alike, a revert's bare `null` as it
        // is.
        let typed = |value: &Value| -> Result<Value> {
            if value.is_null() {
                return Ok(Value::Null);
            }
            let mut held = cell::CellValue::decode(value)?;
            held.r = held.r.min(collection.version);
            let checked = cell::change(collection, key, &held).map_err(|refused| {
                Error::new(&format!(
                    "{}: {refused}; this generation holds the column as the builds that read \
                     it know it",
                    table.role
                ))
            })?;
            checked
                .value
                .ok_or_else(|| Error::new("a typed cell change holds a value"))
        };
        change.value = Some(match cell::carried(value) {
            Some(candidates) => cell::carry(
                &candidates
                    .iter()
                    .map(|(operation, x)| Ok((*operation, typed(x)?)))
                    .collect::<Result<BTreeMap<_, _>>>()?,
            )
            .ok_or_else(|| Error::new("a carried register holds its candidates"))?,
            None => typed(value)?,
        });
    }
    Ok(())
}

/// [`current`] for a read a person waits on: the generation this Mac's catalog names, read
/// under `read_by` with the ordinary admission's wait for each holder (never past `read_by`), and
/// a failure to read it, a busy vault included, the read's own, never the first generation in
/// its place. A home with no Commons custody reads the first generation, as [`current`] does.
///
/// # Errors
/// The catalog cannot be read by `read_by`.
pub(crate) fn current_within(
    paths: &Paths,
    first: &'static Compiled,
    read_by: std::time::Instant,
) -> Result<&'static Compiled> {
    let chain = first.chain();
    if chain.len() == 1 {
        return Ok(first);
    }
    Ok(application::peek_catalog_within(paths, read_by, &|| {
        crate::admission::deadline().min(read_by)
    })?
    .map_or(first, |(trust, snapshot)| {
        current_in(&snapshot, trust, &chain)
    }))
}

/// The compiled table of `table`'s family at `generation`.
fn generation_of(table: &Table, generation: u64) -> Option<&'static Compiled> {
    let target_family = table.role.split('.').next().unwrap_or(table.role);
    generations().find(|compiled| {
        compiled.table.application == table.application
            && compiled
                .table
                .role
                .split('.')
                .next()
                .unwrap_or(compiled.table.role)
                == target_family
            && u64::from(compiled.generation) == generation
    })
}

/// The compiled generation named `<application>/<role>`, of a family in [`FAMILIES`].
///
/// # Errors
/// This build compiles no such table.
pub fn find(name: &str) -> Result<&'static Compiled> {
    generations()
        .find(|compiled| compiled.matches_name(name))
        .ok_or_else(|| {
            Error::new("no typed table of that name in this build (see `aterm db table ls`)")
        })
}

#[cfg(test)]
thread_local! {
    /// How many typed records [`decode`] decoded on this thread: what a test reads to learn
    /// whether a read decoded the table again.
    pub(crate) static DECODED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// A typed record: a canonical `Write` or `Revert`. A write never stops a cell (clearing
/// writes `x: null`, design §3.2), so a bare absence is refused; a revert may restore one.
/// Typed values are judged by the readers, so a newer build's collection or column stays.
pub(crate) fn decode(payload: &[u8]) -> Result<Command> {
    #[cfg(test)]
    DECODED.set(DECODED.get() + 1);
    typed_record(payload, Command::decode(payload)?)
}

/// [`decode`] for a generation that takes version 2 records ([`Compiled::records`]): a
/// version 1 record, or a version 2 one with its statement of who made it ([`crate::said`]),
/// canonical either way.
pub(crate) fn decode_said(payload: &[u8]) -> Result<Command> {
    #[cfg(test)]
    DECODED.set(DECODED.get() + 1);
    typed_record(payload, Command::decode_said(payload)?)
}

/// `command`, decoded from `payload`, as a typed record: canonical, and a write that stops no
/// cell or a revert.
fn typed_record(payload: &[u8], command: Command) -> Result<Command> {
    if command.encode()? != payload {
        return Err(Error::new("typed table payload is not canonical"));
    }
    match &command {
        Command::Write { changes, .. } if changes.iter().all(|c| c.value.is_some()) => Ok(command),
        Command::Revert { .. } => Ok(command),
        Command::Write { .. } => Err(Error::new("a typed table write never stops a cell")),
        _ => Err(Error::new(
            "typed table records are writes and reverts only",
        )),
    }
}

pub use crate::typed::View;

impl View {
    /// `table` as `log` holds it, read through `session`: [`Self::of`] what [`Resolved::read`]
    /// resolves.
    ///
    /// # Errors
    /// As [`Resolved::read`].
    pub(crate) fn read(table: &'static Table, session: &Session<'_>, log: &Log) -> Result<Self> {
        Ok(Self::of(table, log, &Resolved::read(table, session, log)?))
    }

    /// [`Self::read`] over `sealed`, the owned rows and log entries of the generations before
    /// `table`'s that its family keeps, oldest first ([`kept::Rows`], [`Resolved::over`]): the
    /// view a reader of the family is shown. [`Self::read`] is the generation's own log alone,
    /// which is what a checkpoint signs, a roll restates and a quota counts.
    ///
    /// # Errors
    /// As [`Resolved::read`].
    pub(crate) fn read_over(
        table: &'static Table,
        session: &Session<'_>,
        log: &Log,
        sealed: Vec<kept::Rows>,
    ) -> Result<Self> {
        let resolved = Resolved::read(table, session, log)?.over(sealed);
        Ok(Self::of(table, log, &resolved))
    }

    /// `table` as `log` holds it, with what `resolved` read through the session: its cells,
    /// refusals, quota and owned rows, with the records a roll carried into its `archived` rows
    /// and, in a family that keeps its records, the owned rows and log entries `resolved`
    /// holds of the sealed generations before this one ([`kept::overlay`]). It reads nothing
    /// else, so two calls with the same log and the same resolution make the same view.
    pub(crate) fn of(table: &'static Table, log: &Log, resolved: &Resolved) -> Self {
        let (eligible, merges) = log.eligible_and_merges();
        let ops: Vec<Op> = eligible
            .into_iter()
            .filter_map(|(operation, command)| Op::from_eligible(0, operation, command))
            .collect();
        let resolve = |roster: &Digest, actor: &Actor| {
            resolved.bindings.get(&(*roster, *actor)).copied().flatten()
        };
        let (state, over) = State::fold(
            table,
            None,
            &ops,
            &merges,
            resolved.carried_records(),
            log.unwritable(|_| true),
            resolve,
        );
        let quota = owned::Quota {
            spent: state.spent(),
            over,
        };
        let filtered;
        let within: &[Op] = if quota.over.is_empty() {
            &ops
        } else {
            filtered = ops
                .iter()
                .filter(|op| !quota.over.contains(&op.id))
                .cloned()
                .collect::<Vec<Op>>();
            &filtered
        };
        let awaited = log.awaited(resolved.actor);
        let refused = log.refused(&awaited);
        // Whether the cells are exactly the ones the log took from the cells this process keeps
        // of the table: one plain projection, nothing held inert, no claim interpreted.
        let (plain, other) = (std::cell::Cell::new(0_u32), std::cell::Cell::new(false));
        let (mut cells, inert) =
            typed::authority::read(table, within, log.causal_index(), |before, unwritten| {
                if before.is_none() && unwritten.is_empty() {
                    plain.set(plain.get() + 1);
                } else {
                    other.set(true);
                }
                log.cells_without(before, &quota.over, unwritten)
            });
        let mut interpreted = false;
        for collection in table.collections {
            if collection
                .columns
                .iter()
                .any(|column| column.claim.is_some())
                && let std::borrow::Cow::Owned(restored) = typed::claim::interpreted_cells(
                    collection,
                    &cells,
                    within.iter(),
                    log.causal_index(),
                )
            {
                cells = restored;
                interpreted = true;
            }
        }
        let cells_from = (plain.get() == 1
            && !other.get()
            && !interpreted
            && inert.changes.is_empty()
            && inert.carried.is_empty())
        .then(|| log.cells_marked())
        .flatten();
        // A row a Mac wrote in this generation outranks the one a roll carried for it.
        let mut owned: BTreeMap<&'static str, owned::OwnedRead> = table
            .collections
            .iter()
            .filter(|collection| collection.kind == typed::Kind::Owned)
            .map(|collection| (collection.name, state.owned_read(collection.name)))
            .collect();
        // A log's entries as this generation's records hold them ([`State::fold`]).
        let mut logs = state.logs.clone();
        // What the sealed generations before this one hold of each, in a family that keeps its
        // records: a roll restates no owned row and no log entry, so each is read where its
        // signer wrote it.
        kept::overlay(&resolved.sealed, &mut owned, &mut logs);
        // A later generation is whole only once a roll's cover row is here: before that its
        // history is still arriving from the Macs that restated it.
        let restated = table.collection(registry::ROLL.name).is_none()
            || cells
                .keys()
                .any(|(collection, _)| collection == registry::ROLL.name);
        let awaiting = crate::typed::Awaiting {
            records: awaited.len(),
            rows: log.awaited_rows(&awaited),
            restated,
        };
        let refusals = log.refusals(&refused, &resolve);
        Self {
            table: Some(table),
            causal: log.causal_index().clone(),
            cells,
            inert,
            complete: awaited.is_empty() && restated,
            awaiting,
            refused,
            refusals,
            ops,
            quota,
            owned,
            logs,
            state,
            claims_floor: None,
            cells_from,
        }
    }
}

/// What a table view reads through the session, read once: who reads, the records a roll
/// carried that authenticate, and the machine each record's (roster, writer) resolves to.
/// [`View::of`] reads nothing of the session but this, so its digest ([`Self::digest`]), with
/// the log's, fixes the view.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Resolved {
    /// The actor the session reads as: whose records a read still awaits ([`Log::awaited`]).
    actor: Actor,
    /// Each carried record that authenticates, by its operation digest.
    pub(crate) carried: BTreeMap<Digest, CarriedOp>,
    /// The machine each (roster, writer) of an eligible operation or a carried record held a
    /// Writer seat as in that roster; `None` where it held none.
    bindings: BTreeMap<(Digest, Actor), Option<[u8; 16]>>,
    /// The owned rows and log entries of the sealed generations before this one, oldest
    /// first, in a family that keeps its records and for a read that asked for them
    /// ([`Self::over`]); empty otherwise.
    sealed: Vec<kept::Rows>,
}

/// One record a roll carried into a table's `archived` rows, authenticated under the scope of
/// the generation that sealed it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CarriedOp {
    /// The record as the owned rule reads it: its signer, roster and operation digest are the
    /// record's own, and its changes are those of the table's owned collections.
    op: Op,
    /// The eligible operations that carried it: each wrote its wire under the key that names
    /// it ([`typed::keys::archived_pk`]).
    by: std::collections::BTreeSet<Digest>,
}

impl Resolved {
    /// What `table`'s view of `log` reads through `session`. The carried records are those of
    /// every eligible operation that wrote an archived row (design §3.8), past-quota ones
    /// included, read from the operations, not from the cells' current values: any Writer may
    /// write any archived cell, so a later write over a carried record (junk, or an older
    /// genuine record of the same row) must not hide it. Every record that authenticates
    /// stands, and the owned rule ranks them as it ranked them in the generation that sealed
    /// them. A row is left out only for what its own bytes, this build and the history this
    /// Mac holds decide: a record that does not parse or authenticate, one whose roster this
    /// Mac cannot open or whose writer that roster does not seat ([`crate::table::Carried`]),
    /// one whose key does not name its operation, or one whose generation this build does not
    /// compile. The same wire written into several rows is authenticated once, and each
    /// generation's wires in one read batch ([`Session::authenticate_carried`]). Each
    /// record's writer resolves through the roster it names ([`Session::held_binding`]), so a
    /// Mac removed since still owns its rows.
    ///
    /// # Errors
    /// A generation's batch failed for any other reason (a busy vault or store, a membership
    /// that moved while it read, a file or key that did not read), or the session holds no
    /// history of a roster an eligible operation names: the rows are then unknown, never
    /// absent.
    pub(crate) fn read(table: &Table, session: &Session<'_>, log: &Log) -> Result<Self> {
        let carried = carried(
            table,
            session,
            log.eligible_iter()
                .filter(|(_, command)| !matches!(command, Command::Revert { .. }))
                .filter_map(|(operation, command)| Some((operation.header.id, command.changes()?))),
        )?;
        let mut bindings = BTreeMap::new();
        let writers = log
            .eligible_iter()
            .map(|(operation, _)| (operation.roster, operation.header.actor))
            .chain(
                carried
                    .values()
                    .map(|carried| (carried.op.roster, carried.op.actor)),
            );
        for (roster, actor) in writers {
            if let std::collections::btree_map::Entry::Vacant(slot) =
                bindings.entry((roster, actor))
            {
                slot.insert(
                    session
                        .held_binding(&roster, &actor)?
                        .map(|binding| *binding.machine()),
                );
            }
        }
        // A record the view holds and does not project is named by its signer's machine where
        // this session read its roster. One whose payload never authenticated (quarantined, a
        // discarded fork source) names a roster this session may not hold: its signer is then
        // unknown, never a reason the read fails (a record this build cannot read is inert, not
        // a stop).
        for (roster, actor) in log.refused_writers() {
            if let std::collections::btree_map::Entry::Vacant(slot) =
                bindings.entry((roster, actor))
            {
                slot.insert(
                    session
                        .held_binding(&roster, &actor)
                        .ok()
                        .flatten()
                        .map(|binding| *binding.machine()),
                );
            }
        }
        Ok(Self {
            actor: session.actor(),
            carried,
            bindings,
            sealed: Vec::new(),
        })
    }

    /// Each carried record that authenticates, with the records that carried it.
    pub(crate) fn carried_records(&self) -> impl Iterator<Item = (&Op, &BTreeSet<Digest>)> {
        self.carried
            .values()
            .map(|carried| (&carried.op, &carried.by))
    }

    /// `self` over `sealed`, the owned rows and log entries of the sealed generations before
    /// this one, oldest first ([`kept::rows`]): [`View::of`] lays them under its own.
    #[must_use]
    pub(crate) fn over(mut self, sealed: Vec<kept::Rows>) -> Self {
        self.sealed = sealed;
        self
    }

    /// A digest of the whole resolution: equal digests resolve every record alike.
    pub(crate) fn digest(&self) -> [u8; 32] {
        // Every field takes a position: a new one does not compile until it does.
        let Self {
            actor,
            carried,
            bindings,
            sealed,
        } = self;
        let mut sha = aterm_digest::Sha256::new();
        sha.update(b"aterm/db/resolved/1\0");
        sha.update(actor);
        sha.update((carried.len() as u64).to_le_bytes());
        for (id, CarriedOp { op: _, by }) in carried {
            // The op is the record's, which its id (the digest of the complete authenticated
            // record) fixes, read under this build's table.
            sha.update(id);
            sha.update((by.len() as u64).to_le_bytes());
            for carrier in by {
                sha.update(carrier);
            }
        }
        sha.update((bindings.len() as u64).to_le_bytes());
        for ((roster, writer), machine) in bindings {
            sha.update(roster);
            sha.update(writer);
            match machine {
                None => sha.update([0]),
                Some(machine) => {
                    sha.update([1]);
                    sha.update(machine);
                }
            }
        }
        // A resolution over no sealed generation digests as it did before a view read any:
        // the fields above are each counted, so this section starts where they end.
        if !sealed.is_empty() {
            sha.update(b"sealed\0");
            sha.update((sealed.len() as u64).to_le_bytes());
            for rows in sealed {
                rows.digest_into(&mut sha);
            }
        }
        sha.finalize()
    }

    /// `self` with `machine` as what `(roster, writer)` resolves to: a test's second
    /// resolution that differs from the first in one binding alone.
    #[cfg(test)]
    pub(crate) fn rebound(
        mut self,
        roster: Digest,
        writer: Actor,
        machine: Option<[u8; 16]>,
    ) -> Self {
        self.bindings.insert((roster, writer), machine);
        self
    }

    /// Each (roster, writer) this resolution holds a binding for.
    #[cfg(test)]
    pub(crate) fn writers(&self) -> impl Iterator<Item = &(Digest, Actor)> {
        self.bindings.keys()
    }
}

/// The records a roll carried into `table`'s `archived` rows that authenticate, by operation
/// digest, from the eligible operations `ops` (each one's digest and changes): [`Resolved::read`].
///
/// # Errors
/// As [`Resolved::read`].
fn carried<'a>(
    table: &Table,
    session: &Session<'_>,
    ops: impl Iterator<Item = (Digest, &'a [Change])>,
) -> Result<BTreeMap<Digest, CarriedOp>> {
    use std::collections::BTreeSet;
    let Some(collection) = table.collection(registry::ARCHIVED.name) else {
        return Ok(BTreeMap::new());
    };
    let trust = session.trust();
    // Each distinct carried wire, in the order its first row names it, with every key it was
    // written under and the operations that wrote it there; the wires of one generation's scope
    // are authenticated together.
    type Keys = BTreeMap<String, BTreeSet<Digest>>;
    let mut wires: Vec<(crate::table::Scope, Vec<u8>, Keys)> = Vec::new();
    // Each wire's record is read as its own generation reads its records.
    let mut decoders: Vec<fn(&[u8]) -> Result<Command>> = Vec::new();
    let mut first: BTreeMap<(u64, Vec<u8>), usize> = BTreeMap::new();
    for (op, changes) in ops {
        // One archived row's cells go in one record (`pack` never splits a row).
        let mut rows: BTreeMap<&str, (Option<u64>, Option<String>)> = BTreeMap::new();
        for change in changes
            .iter()
            .filter(|change| change.collection == collection.name)
        {
            let Ok(key) = cell::CellKey::parse(&change.key) else {
                continue;
            };
            let Some(value) = change
                .value
                .as_ref()
                .and_then(|value| cell::CellValue::decode(value).ok())
            else {
                continue;
            };
            let slot = rows.entry(key.pk).or_default();
            match (key.col, key.elem) {
                ("generation", None) => slot.0 = value.x.as_u64(),
                ("wire", None) => {
                    if let Value::String(wire) = value.x {
                        slot.1 = Some(wire);
                    }
                }
                _ => {}
            }
        }
        for (pk, (generation, wire)) in rows {
            let (Some(generation), Some(wire)) = (generation, wire) else {
                continue;
            };
            let Some(compiled) = generation_of(table, generation) else {
                continue;
            };
            let (Some(wire), Ok(descriptor)) = (
                aterm_secrets::b64::decode_padded(&wire),
                compiled.descriptor(trust),
            ) else {
                continue;
            };
            let at = *first.entry((generation, wire.clone())).or_insert_with(|| {
                wires.push((descriptor.scope, wire, Keys::new()));
                decoders.push(compiled.decoder());
                wires.len() - 1
            });
            wires[at].2.entry(pk.to_owned()).or_default().insert(op);
        }
    }
    #[cfg(test)]
    crate::file::seam::carried_read();
    let mut records: Vec<Option<crate::table::Record>> = wires.iter().map(|_| None).collect();
    let mut scopes: Vec<crate::table::Scope> = Vec::new();
    for (scope, _, _) in &wires {
        if !scopes.contains(scope) {
            scopes.push(*scope);
        }
    }
    for scope in scopes {
        let (at, of): (Vec<usize>, Vec<&[u8]>) = wires
            .iter()
            .enumerate()
            .filter(|(_, (wire_scope, _, _))| *wire_scope == scope)
            .map(|(at, (_, wire, _))| (at, wire.as_slice()))
            .unzip();
        #[cfg(test)]
        crate::table::CARRIED_AUTHENTICATED
            .set(crate::table::CARRIED_AUTHENTICATED.get() + of.len());
        for (at, answer) in at
            .into_iter()
            .zip(session.authenticate_carried(scope, &of)?)
        {
            if let crate::table::Carried::Record(record) = answer {
                records[at] = Some(*record);
            }
        }
    }
    let mut out: BTreeMap<Digest, CarriedOp> = BTreeMap::new();
    for (((_, _, keys), record), decode) in wires.iter().zip(records).zip(&decoders) {
        let Some(record) = record else {
            continue;
        };
        let header = record.header();
        // Only a row whose key names the record carries it.
        let Some(by) = keys.get(typed::keys::archived_pk(&header.id).as_str()) else {
            continue;
        };
        if let Some(held) = out.get_mut(&header.id) {
            held.by.extend(by);
            continue;
        }
        let Some(changes) = decode(record.payload())
            .ok()
            .and_then(Command::into_changes)
        else {
            continue;
        };
        out.insert(
            header.id,
            CarriedOp {
                op: Op {
                    id: header.id,
                    actor: header.actor,
                    sequence: header.sequence,
                    roster: *record.roster(),
                    period: 0,
                    revert: None,
                    changes: changes
                        .into_iter()
                        .filter(|change| {
                            table
                                .collection(&change.collection)
                                .is_some_and(|c| c.kind == typed::Kind::Owned)
                        })
                        .collect(),
                },
                by: by.clone(),
            },
        );
    }
    Ok(out)
}

/// The records a roll carried that an operation of `ops` carried ([`Resolved::carried`]), in
/// operation digest order, each with the operation of `ops` that archived it: the first, in
/// `ops`' order, whose row authenticates as that record. A record no operation of `ops` carried
/// is left out. It reads nothing of the session: [`Resolved::read`] authenticated each record
/// once.
pub(crate) fn archived_records(resolved: &Resolved, ops: &[Op]) -> Vec<(Digest, Op)> {
    let place: BTreeMap<&Digest, usize> = ops
        .iter()
        .enumerate()
        .map(|(at, op)| (&op.id, at))
        .collect();
    resolved
        .carried
        .values()
        .filter_map(|carried| {
            let first = carried.by.iter().filter_map(|id| place.get(id)).min()?;
            Some((ops[*first].id, carried.op.clone()))
        })
        .collect()
}

/// Pack rows of changes into records of at most [`MAX_WRITES`] changes and a payload within
/// the Commons body limit, never splitting a row.
///
/// # Errors
/// One row alone passes a record's limits.
pub(crate) fn pack(rows: Vec<Vec<Change>>) -> Result<Vec<Vec<Change>>> {
    pack_saying(rows, None)
}

/// [`pack`] for records that each carry `said` ([`crate::said`]), which takes its bytes in
/// every record.
///
/// # Errors
/// One row alone passes a record's limits.
pub(crate) fn pack_saying(rows: Vec<Vec<Change>>, said: Option<&Said>) -> Result<Vec<Vec<Change>>> {
    // `Command::Write::encode()` wraps the serialized `changes` JSON array (`[...]`) in an
    // envelope of fixed size for one statement: 78 bytes for a version 1 record,
    // `{"changes":...,"kind":"write","request":"<32-hex>","v":1}`.
    let envelope = Command::Write {
        request: [0; 16],
        changes: Vec::new(),
        said: said.cloned().map(Box::new),
    }
    .encode()?
    .len()
        - "[]".len();
    let mut records: Vec<Vec<Change>> = Vec::new();
    let mut last_cmd_bytes = 0usize;
    for row in rows {
        if row.len() > MAX_WRITES {
            return Err(Error::new("one typed row passes a record's limits"));
        }
        let row_json_len = aterm_json::to_vec(&row)
            .map_err(|_| Error::new("one typed row passes a record's limits"))?
            .len();
        let row_cmd_bytes = envelope + row_json_len;
        if row_cmd_bytes > crate::crypto::MAX_BODY {
            return Err(Error::new("one typed row passes a record's limits"));
        }
        match records.last_mut() {
            Some(last) if last.len() + row.len() <= MAX_WRITES => {
                let combined_cmd_bytes = if last.is_empty() {
                    row_cmd_bytes
                } else if row.is_empty() {
                    last_cmd_bytes
                } else {
                    last_cmd_bytes + row_json_len - 1
                };
                if combined_cmd_bytes <= crate::crypto::MAX_BODY {
                    last.extend(row);
                    last_cmd_bytes = combined_cmd_bytes;
                } else {
                    last_cmd_bytes = row_cmd_bytes;
                    records.push(row);
                }
            }
            _ => {
                last_cmd_bytes = row_cmd_bytes;
                records.push(row);
            }
        }
    }
    Ok(records)
}

/// What [`Handle::save`] came to.
#[derive(Clone, Debug, PartialEq)]
pub enum Saved {
    /// Written: the plan read from the text, and what the write did.
    Written { plan: Box<Plan>, applied: Applied },
    /// Nothing written: where and why, never a value.
    Problems(Vec<grid::Problem>),
}

/// What a grid save wrote.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Applied {
    /// Records appended.
    pub records: usize,
    /// Registers written.
    pub cells: usize,
    /// Edits the table already held.
    pub unchanged: usize,
    /// Cells that changed since the export: left alone.
    pub stale: Vec<Stale>,
    /// Keys of born rows written by this save.
    pub born: Vec<String>,
    /// Keys of rows retired or ended by this save.
    pub retired: Vec<String>,
    /// The operations appended.
    pub operations: Vec<Digest>,
    /// The table's rules refused the save ([`Compiled::rules`]): nothing of it was written.
    pub refused: Option<Refused>,
}

/// What a table's capacity asks of the owner (design §3.9), as `check` reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Capacity {
    /// A writer's automatic records, with the merges they cost, reached the table's quota:
    /// its later facts, observations and births are inert until the table rolls.
    QuotaSpent { writer: Actor, spent: usize },
    /// Roll the table to its next generation (`registry.2`, step 13).
    Roll(Roll),
}

/// Why a table should roll.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Roll {
    /// A writer's quota is spent; only a new generation gives it room again.
    Quota,
    /// The table holds [`ROLL_AT_PERCENT`] of the store's records.
    Records,
    /// The table holds [`ROLL_AT_PERCENT`] of the store's bytes.
    Bytes,
}

/// The share of the store's records or bytes at which a table should roll.
pub const ROLL_AT_PERCENT: usize = 75;

/// What `view`'s quota and the table's `usage` ask of the owner: each writer whose quota is
/// spent, and a roll once any is, or once the table holds [`ROLL_AT_PERCENT`] of the store's
/// records or bytes. Automatic writes alone never reach 75 % (128 Macs at 24 records is about
/// a third), so a spent quota is a roll trigger of its own.
pub fn capacity(table: &Table, view: &View, usage: &Usage) -> Vec<Capacity> {
    use crate::table::store::{MAX_BYTES, MAX_RECORDS};
    let mut found: Vec<Capacity> = view
        .quota
        .spent
        .iter()
        .filter(|(_, spent)| **spent >= table.self_quota)
        .map(|(writer, spent)| Capacity::QuotaSpent {
            writer: *writer,
            spent: *spent,
        })
        .collect();
    let roll = if !found.is_empty() {
        Some(Roll::Quota)
    } else if usage.records * 100 >= MAX_RECORDS * ROLL_AT_PERCENT {
        Some(Roll::Records)
    } else if usage.bytes * 100 >= MAX_BYTES * ROLL_AT_PERCENT {
        Some(Roll::Bytes)
    } else {
        None
    };
    found.extend(roll.map(Capacity::Roll));
    found
}

/// What the store lets one table hold here, which `aterm db table ls` reports a table's records
/// and bytes against: in all, and authored by one writer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caps {
    pub records: usize,
    pub bytes: usize,
    pub writer_records: usize,
    pub writer_bytes: usize,
}

/// The store's [`Caps`].
pub const CAPS: Caps = {
    use crate::table::store::{MAX_BYTES, MAX_RECORDS, WRITER_BYTES, WRITER_RECORDS};
    Caps {
        records: MAX_RECORDS,
        bytes: MAX_BYTES,
        writer_records: WRITER_RECORDS,
        writer_bytes: WRITER_BYTES,
    }
};

/// The records and bytes a table holds here, in all and authored by this Mac.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Usage {
    /// This Mac's writer in the table.
    pub actor: Actor,
    pub records: usize,
    pub bytes: usize,
    pub mine_records: usize,
    pub mine_bytes: usize,
}

impl Usage {
    fn of(actor: Actor, log: &Log) -> Self {
        let used = log.local_use();
        Self {
            actor,
            records: used.records,
            bytes: used.bytes,
            mine_records: used.writer_records,
            mine_bytes: used.writer_bytes,
        }
    }
}

/// The sealer of table `name` in `session` ([`typed::secret::Sealer`]), for a generation that
/// holds sealed tokens: the current audience epoch seals, and a stored token opens under the
/// epoch it names, through the session's retained handoffs.
///
/// # Errors
/// The current audience key is unavailable.
pub(crate) fn sealer<'s>(
    name: &'s str,
    session: &'s Session<'_>,
    scope: crate::table::Scope,
) -> Result<impl typed::secret::Sealer + 's> {
    let epoch = session.secret_basis();
    let key = session.secret_key(scope, &epoch)?;
    let cache: std::cell::RefCell<BTreeMap<Digest, zeroize::Zeroizing<[u8; 32]>>> =
        std::cell::RefCell::new(BTreeMap::new());
    Ok(typed::secret::Keyed {
        table: name,
        epoch,
        key,
        resolve: move |basis: &Digest| {
            if let Some(cached) = cache.borrow().get(basis) {
                return Ok(cached.clone());
            }
            let resolved = session.secret_key(scope, basis)?;
            cache.borrow_mut().insert(*basis, resolved.clone());
            Ok(resolved)
        },
    })
}

/// Observe the table again while an observation answers busy, until `deadline`: a writer that
/// advanced the table mid-read (a repair round landing, another Mac's edit arriving) makes the
/// observer refuse its result and ask for a fresh observation ([`Error::snapshot_changed`]), so a
/// read racing a sync takes the newer table instead of failing. Any other answer, and a busy one
/// once `deadline` passed, is returned as it is. Only pure reads come here: each attempt is a
/// whole new observation, and none writes.
///
/// # Errors
/// What the last attempt answered.
pub(crate) fn reopened<T>(
    deadline: std::time::Instant,
    mut observe: impl FnMut() -> Result<T>,
) -> Result<T> {
    loop {
        match observe() {
            Err(error) if error.is_busy() && std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            answered => return answered,
        }
    }
}

/// A typed table on this Mac's Commons.
pub struct Handle {
    paths: Paths,
    /// The family's generation this handle was asked for: the first of the chain its own
    /// generation is of ([`Self::at`]), whose earlier generations a read across a roll opens
    /// ([`kept`]).
    family: &'static Compiled,
    compiled: &'static Compiled,
    /// Who makes this handle's writes, why and on what evidence ([`Self::saying`]).
    said: Option<Box<Said>>,
}

impl Handle {
    /// The table `compiled` names, at the generation this Mac's catalog says to use
    /// ([`current`]): after a roll, `alab/registry` reaches `alab/registry.2`.
    pub fn at(paths: Paths, compiled: &'static Compiled) -> Self {
        let family = compiled;
        let compiled = current(&paths, compiled);
        Self {
            paths,
            family,
            compiled,
            said: None,
        }
    }

    /// This handle, its writes saying who made them, why and on what evidence ([`crate::said`]):
    /// each record carries `said` where the generation it writes takes version 2 records
    /// ([`Compiled::carries`]), and none where it takes version 1 only.
    #[must_use]
    pub fn saying(mut self, said: Said) -> Self {
        self.said = Some(Box::new(said));
        self
    }

    /// What this handle's records carry of who makes them ([`Self::saying`]).
    fn carried(&self) -> Option<Box<Said>> {
        self.compiled.carries(self.said.as_deref())
    }

    /// [`Self::at`] for a read a person waits on: the generation is read as [`current_within`]
    /// reads it, observed again while it meets a holder, until `read_by`.
    ///
    /// # Errors
    /// This Mac's catalog cannot be read by `read_by`.
    pub fn at_until(
        paths: Paths,
        compiled: &'static Compiled,
        read_by: std::time::Instant,
    ) -> Result<Self> {
        let family = compiled;
        let compiled = reopened(read_by, || current_within(&paths, compiled, read_by))?;
        Ok(Self {
            paths,
            family,
            compiled,
            said: None,
        })
    }

    /// The table this handle reaches.
    pub const fn compiled(&self) -> &'static Compiled {
        self.compiled
    }

    /// Whether `first`'s family has one generation here, and it is the one this handle reaches
    /// ([`sole_generation`]).
    pub fn sole_generation(&self, first: &'static Compiled) -> bool {
        sole_generation(&self.paths, first, self.compiled)
    }

    fn descriptor(&self) -> impl FnOnce(&Bootstrap) -> Result<Descriptor> + use<> {
        let table = self.compiled.table;
        let version = self.compiled.version;
        move |bootstrap| {
            Descriptor::application(bootstrap.trust(), table.application, table.role, version)
        }
    }

    /// Read the table with the sealer of its sealed tokens, writing nothing: a sign-in on a
    /// generation builds up to 0.200.0 read, whose secret cells hold sealed tokens
    /// ([`typed::Ty::Secret`]) rather than store references, and the importer's dry run, which
    /// compares a held number and a held secret there by opening its token.
    ///
    /// # Errors
    /// As [`Self::view`], or this Mac holds no key of the table's audience.
    pub(crate) fn view_sealed<T>(
        &self,
        read: impl FnOnce(&View, &dyn typed::secret::Sealer) -> Result<T>,
    ) -> Result<T> {
        let compiled = self.compiled;
        application::observe_store(&self.paths, self.descriptor(), |session, store, scope| {
            let view = View::read(
                compiled.table,
                session,
                &Log::read_with(session, store, scope, self.compiled.decoder())?,
            )?;
            let name = compiled.name();
            read(&view, &sealer(&name, session, scope)?)
        })
    }

    /// Read the table this Mac holds, without enrolling it.
    ///
    /// # Errors
    /// No Commons, the table is not declared or not subscribed here, its storage is damaged, or
    /// the view's resolution refuses (`Resolved::read`'s errors).
    pub fn view(&self) -> Result<View> {
        self.view_at(Point::Head)
    }

    /// Why this Mac cannot write the table now, where it cannot, in words that follow "cannot
    /// write the table:": it is not a Commons writer here, its writer succession is still pending
    /// (the home is fenced while its successor preserves the old records), or its history here is
    /// incomplete in `view`, the head as this Mac read it (a record waits on an earlier one, so a
    /// write may meet a value not yet read). `None` where a write may go ahead: the store still
    /// judges each one.
    pub fn cannot_write(&self, view: &View) -> Option<String> {
        if !aterm_secrets::commons::bootstrap::writer_here(&self.paths) {
            return Some("it is not a Commons writer here".to_owned());
        }
        if crate::generation::fenced(&self.paths).unwrap_or(false) {
            return Some("its writer succession is still pending".to_owned());
        }
        if !view.complete {
            return Some(match view.awaiting.records {
                0 => "its history here is incomplete: this generation's restated history is \
                      still arriving"
                    .to_owned(),
                1 => "its history here is incomplete: 1 record waits on an earlier one".to_owned(),
                n => format!("its history here is incomplete: {n} records wait on earlier ones"),
            });
        }
        None
    }

    /// [`Self::view`] as of `point`: the table as the causal cut an operation names projects it,
    /// through the same read, quota and claims included, with eligibility judged on everything
    /// this Mac retains now. In a family that keeps its records across a roll ([`kept`]), a
    /// point of a sealed generation is read from that generation's own log, as its table, and
    /// every point holds the owned rows and log entries of the sealed generations before its
    /// own, each read from its generation's log ([`kept::Rows`]).
    ///
    /// # Errors
    /// As [`Self::view`], or this Mac retains no operation `point` names in this table; for a
    /// family that keeps its records, also where this Mac does not hold a sealed generation yet.
    pub fn view_at(&self, point: Point) -> Result<View> {
        let table = self.compiled.table;
        let earlier = self.earlier();
        reopened(crate::admission::deadline(), || {
            application::observe_store(&self.paths, self.descriptor(), |session, store, scope| {
                let log = Log::read_with(session, store, scope, self.compiled.decoder())?;
                let at = match point {
                    Point::Head => None,
                    Point::At(at) => Some(at),
                };
                // A point of a sealed generation a family keeps is that generation's table
                // at the cut ([`kept::as_of`]).
                if let Some(at) = at.filter(|at| !earlier.is_empty() && !log.holds(at))
                    && let Some(view) = kept::as_of(session, &earlier, &at)?
                {
                    return Ok(view);
                }
                let sealed = kept::rows(session, &kept::sealed(session, &earlier)?)?;
                match at {
                    None => View::read_over(table, session, &log, sealed),
                    Some(at) => View::read_over(table, session, &log.as_of(&at)?, sealed),
                }
            })
        })
    }

    /// What changed from the cut `from` names to `to`'s: each cell added, retired, changed or
    /// conflict-changed, and each owned row, with the operations of `to` that made it, their
    /// writers and machines. Both sides are read in one observation through [`Self::view_at`]'s
    /// projection. A comparison runs forward: `to` must have observed `from` (the head holds
    /// every operation this Mac retains); two concurrent points are each compared with the head.
    /// In a family that keeps its records across a roll, `from` may be a point of a sealed
    /// generation, and each difference names the record that made it in whichever generation's
    /// log holds it, with what that record says of who made it ([`kept::diff`]).
    ///
    /// # Errors
    /// As [`Self::view_at`] for either point, or `to` did not observe `from`.
    pub fn diff(&self, from: Digest, to: Point) -> Result<Diff> {
        let table = self.compiled.table;
        let earlier = self.earlier();
        reopened(crate::admission::deadline(), || {
            application::observe_store(&self.paths, self.descriptor(), |session, store, scope| {
                let log = Log::read_with(session, store, scope, self.compiled.decoder())?;
                // From a point of a sealed generation a family keeps, across the roll
                // ([`kept::diff`]).
                if !earlier.is_empty()
                    && !log.holds(&from)
                    && let Some(diff) =
                        kept::diff(session, self.compiled, &log, &earlier, from, to)?
                {
                    return Ok(diff);
                }
                let past = log.as_of(&from)?;
                let later = match to {
                    Point::Head => None,
                    Point::At(at) => Some(log.as_of(&at)?),
                };
                let later = later.as_ref().unwrap_or(&log);
                if !later.holds(&from) {
                    return Err(Error::new(
                        "the later point did not observe the first: compare each with head",
                    ));
                }
                // Both points hold what the sealed generations before this one hold of each
                // owned row and log entry, so a row this generation wrote over a sealed one
                // reads as changed, and one it never wrote does not differ.
                let sealed = kept::rows(session, &kept::sealed(session, &earlier)?)?;
                let before = View::read_over(table, session, &past, sealed.clone())?;
                let after = View::read_over(table, session, later, sealed)?;
                let resolve = owned::commons::historical(session);
                let cells = diff::between(table, &before, &after, |id| {
                    let operation = log.operation(id)?;
                    Some(Maker {
                        operation: *id,
                        actor: operation.header.actor,
                        machine: resolve(&operation.roster, &operation.header.actor),
                        said: log.said(id).cloned(),
                    })
                });
                Ok(Diff {
                    from,
                    to,
                    cells,
                    refused: after.refusals.clone(),
                })
            })
        })
    }

    /// [`Self::view`] for a read a person waits on, such as the resolution of a fleet label. Each
    /// wait for a holder of the vault or of custody is the ordinary admission's
    /// ([`crate::admission::deadline`], asked as each such stage starts), so a holder keeps this read
    /// waiting no longer than any other; the reading itself may run until `read_by`, so a loaded
    /// Mac's pace, or a successor home's predecessors read beside its own custody, is no refusal;
    /// and a read that met a holder is observed again until then.
    ///
    /// # Errors
    /// As [`Self::view`], or the read ran out of time at `read_by`.
    pub fn view_until(&self, read_by: std::time::Instant) -> Result<View> {
        let table = self.compiled.table;
        let earlier = self.earlier();
        reopened(read_by, || {
            application::observe_store_within(
                &self.paths,
                self.descriptor(),
                read_by,
                &|| crate::admission::deadline().min(read_by),
                |session, store, scope| {
                    View::read_over(
                        table,
                        session,
                        &Log::read_with(session, store, scope, self.compiled.decoder())?,
                        kept::rows(session, &kept::sealed(session, &earlier)?)?,
                    )
                },
            )
        })
    }

    /// The view and what the table's storage holds here.
    ///
    /// # Errors
    /// As [`Self::view`].
    pub fn usage(&self) -> Result<(View, Usage)> {
        self.usage_with(|_, _| ())
            .map(|(view, usage, ())| (view, usage))
    }

    /// [`Self::usage`], and with it what `of` reads of the same log under the session that read
    /// it: one read of the table serves both (the checkpoint fence's read,
    /// [`Self::usage_fenced`]).
    ///
    /// # Errors
    /// As [`Self::view`].
    pub(crate) fn usage_with<T>(
        &self,
        of: impl Fn(&crate::table::Session<'_>, &Log) -> T,
    ) -> Result<(View, Usage, T)> {
        let table = self.compiled.table;
        let earlier = self.earlier();
        reopened(crate::admission::deadline(), || {
            application::observe_store(&self.paths, self.descriptor(), |session, store, scope| {
                let log = Log::read_with(session, store, scope, self.compiled.decoder())?;
                let sealed = kept::rows(session, &kept::sealed(session, &earlier)?)?;
                Ok((
                    View::read_over(table, session, &log, sealed)?,
                    Usage::of(session.actor(), &log),
                    of(session, &log),
                ))
            })
        })
    }

    /// The history a roll carried into this generation (`alab::history`): each entry of an
    /// earlier generation's log with its operation, writer, machine, clocks and changes, a
    /// secret's value withheld, and each horizon a roll's bound stated. Empty for a generation no
    /// roll wrote into.
    ///
    /// # Errors
    /// As [`Self::view`].
    pub fn carried_history(&self) -> Result<crate::alab::history::Carried> {
        let table = self.compiled.table;
        reopened(crate::admission::deadline(), || {
            application::observe_store(&self.paths, self.descriptor(), |session, store, scope| {
                let log = Log::read_with(session, store, scope, self.compiled.decoder())?;
                Ok(crate::alab::history::carried(
                    &View::read(table, session, &log)?,
                    owned::commons::historical(session),
                    None,
                ))
            })
        })
    }

    /// Write a grid save (design §8): under the store's writer, re-read the table and settle
    /// `plan` against it ([`grid::settle`]): each changed register is written only while its
    /// candidates are still the exported ones, a cell that changed meanwhile is left alone and
    /// returned as stale, what is left passes the table's rules ([`Compiled::rules`]), and one
    /// row's changes go in one owner record (at most 64 changes, a fresh random request id each).
    /// A save the rules refuse writes nothing and comes back with [`Applied::refused`] set, for
    /// the grid to show at that row as it shows a problem.
    ///
    /// # Errors
    /// No Commons writer; a row the save writes is one an operation still waiting on a causal
    /// parent writes, or such an operation cannot be read here (either could hold a candidate the
    /// comparison cannot see, [`View::holds_back`]); or a record cannot be written.
    pub fn apply(&self, collection: &Collection, plan: &Plan) -> Result<Applied> {
        let table = self.compiled.table;
        application::with_store(
            &self.paths,
            false,
            self.descriptor(),
            |session, store, scope| {
                session.writer()?;
                let log = Log::read_with(session, store, scope, self.compiled.decoder())?;
                log.writable(session.actor())?;
                let view = View::read(table, session, &log)?;
                let mut settled = grid::settle(collection, plan, &view.cells);
                if let Some(refused) =
                    self.compiled
                        .rules(&mut settled.rows, &view, Author::Owner)?
                {
                    return Ok(Applied {
                        refused: Some(refused),
                        ..Applied::default()
                    });
                }
                // What waits is judged by each change's row, which the form [`conform`] writes
                // the change in keeps.
                if let Some(why) = view.holds_back(&settled.rows) {
                    return Err(Error::new(&format!(
                        "this table's {why}; a grid save of that row waits for it"
                    )));
                }
                let mut applied = Applied {
                    unchanged: settled.unchanged,
                    stale: settled.stale,
                    born: settled.born,
                    retired: settled.retired,
                    ..Applied::default()
                };
                let said = self.carried();
                for row in &mut settled.rows {
                    conform(table, row)?;
                }
                for changes in self.compiled.packed(settled.rows, said.as_deref())? {
                    let count = changes.len();
                    let command = Command::Write {
                        request: crate::crypto::random()?,
                        changes,
                        said: said.clone(),
                    };
                    self.compiled.admits(&command)?;
                    let receipt = application::append(session, store, &command.encode()?)?;
                    applied.records += 1;
                    applied.cells += count;
                    applied.operations.push(receipt.id);
                }
                Ok(applied)
            },
        )
    }

    /// Write rows `derive` computes from the table as it is under the store's writer (the lab
    /// importer's three-way plan, design §9), declaring and enrolling the table when `declare`:
    /// the rows pass the rules every table keeps ([`Compiled::rules`], [`Author::Derived`]), and
    /// the plan is written only where no withheld operation writes a row it writes
    /// ([`View::holds_back`]), so a withheld operation can never hide an owner's edit from the
    /// comparison; its rows are packed into records of at most 64 changes, each with a
    /// content-derived request id, so a retry returns its first receipt and a re-run of the same
    /// plan writes nothing.
    ///
    /// # Errors
    /// No Commons writer; the rules refuse the rows; a row the plan writes may still change under
    /// it here ([`View::holds_back`]); `derive` fails; or a record cannot be written.
    pub(crate) fn commit_derived<T>(
        &self,
        declare: bool,
        derive: impl FnOnce(&View) -> Result<(T, Vec<Vec<Change>>)>,
    ) -> Result<(T, Applied)> {
        self.commit_planned(declare, false, crate::admission::deadline(), |view, _| {
            derive(view)
        })
    }

    /// [`Self::commit_derived`] held to `deadline`, for a command a person runs and waits on (a
    /// product open's budget is no measure of a pass that has to happen), whose plan may seal and
    /// compare the cells a generation holds sealed (`typed::secret::Sealer`): the importer's
    /// `number` cells in a generation builds up to 0.200.0 read. The audience key stays inside
    /// the session that writes the plan.
    ///
    /// # Errors
    /// As [`Self::commit_derived`], the table's key is unavailable, or `deadline` passes first.
    pub(crate) fn commit_sealed_until<T>(
        &self,
        declare: bool,
        deadline: std::time::Instant,
        derive: impl FnOnce(&View, &dyn typed::secret::Sealer) -> Result<(T, Vec<Vec<Change>>)>,
    ) -> Result<(T, Applied)> {
        self.commit_planned(declare, false, deadline, derive)
    }

    /// [`Self::commit_derived`] for the owner's own word (`aterm db alab add|retire|relabel`): each
    /// record takes a random request id, as a grid save does, so the owner saying a value again
    /// after saying another (a relabel back and forth, a retire undone and redone) is written,
    /// never taken for a retry of the first record. A repeat of a value the table holds writes
    /// nothing anyway: the verbs write only the cells that change. The rows pass the table's
    /// rules first ([`Compiled::rules`]), as a grid save's do.
    ///
    /// # Errors
    /// As [`Self::commit_derived`], or the table's rules refuse the rows.
    pub(crate) fn commit_owner<T>(
        &self,
        derive: impl FnOnce(&View) -> Result<(T, Vec<Vec<Change>>)>,
    ) -> Result<(T, Applied)> {
        self.commit_planned(false, true, crate::admission::deadline(), |view, _| {
            derive(view)
        })
    }

    /// [`Self::commit_owner`] with the table's sealer: the owner's own word that writes a cell a
    /// generation holds sealed (`aterm db phones set <row> number <value>` in a generation builds
    /// up to 0.200.0 read), sealed inside the writing session under its current epoch. Each
    /// record's request id is random.
    ///
    /// # Errors
    /// As [`Self::commit_owner`], or the table's key is unavailable.
    pub(crate) fn commit_owner_sealed<T>(
        &self,
        derive: impl FnOnce(&View, &dyn typed::secret::Sealer) -> Result<(T, Vec<Vec<Change>>)>,
    ) -> Result<(T, Applied)> {
        self.commit_planned(false, true, crate::admission::deadline(), derive)
    }

    /// Open each of `cells` (`(collection, row, column, token)`) in one store session. A cell
    /// whose epoch this Mac holds no key of, or whose token does not open at its coordinates,
    /// comes back `None`. Only a column a generation holds sealed though its values are ordinary
    /// data (a phone's `number`) is opened this way ([`open_ordinary`]): nothing is journaled,
    /// since no secret is revealed.
    ///
    /// # Errors
    /// The store cannot be read.
    pub(crate) fn open_cells(
        &self,
        cells: &[(&str, &str, &str, &str)],
    ) -> Result<Vec<Option<typed::secret::Secret>>> {
        let table = self.compiled.name();
        application::observe_store(&self.paths, self.descriptor(), |session, _, scope| {
            let mut keys: BTreeMap<Digest, Option<zeroize::Zeroizing<[u8; 32]>>> = BTreeMap::new();
            Ok(cells
                .iter()
                .map(|&(collection, row, column, token)| {
                    let cell = typed::secret::Cell {
                        table: &table,
                        collection,
                        row,
                        column,
                    };
                    let epoch = typed::secret::epoch(token).ok()?;
                    let key = keys
                        .entry(epoch)
                        .or_insert_with(|| session.secret_key(scope, &epoch).ok())
                        .as_ref()?;
                    typed::secret::open(token, key, cell).ok()
                })
                .collect())
        })
    }

    /// The plan `derive` makes, written once its rows pass the table's rules
    /// ([`Compiled::rules`]): `owner` is the owner's own word, whose records take random request
    /// ids and whose rows pass the table's guard too; otherwise each record's request id derives
    /// from its content, so a retry returns its first receipt.
    fn commit_planned<T>(
        &self,
        declare: bool,
        owner: bool,
        deadline: std::time::Instant,
        derive: impl FnOnce(&View, &dyn typed::secret::Sealer) -> Result<(T, Vec<Vec<Change>>)>,
    ) -> Result<(T, Applied)> {
        let table = self.compiled.table;
        let name = self.compiled.name();
        application::with_store_until(
            &self.paths,
            declare,
            self.descriptor(),
            deadline,
            |session, store, scope| {
                session.writer()?;
                let log = Log::read_with(session, store, scope, self.compiled.decoder())?;
                log.writable(session.actor())?;
                let view = View::read(table, session, &log)?;
                let (planned, mut rows) = derive(&view, &sealer(&name, session, scope)?)?;
                let author = if owner {
                    Author::Owner
                } else {
                    Author::Derived
                };
                if let Some(refused) = self.compiled.rules(&mut rows, &view, author)? {
                    return Err(refused.into());
                }
                if let Some(why) = view.holds_back(&rows) {
                    return Err(Error::new(&format!(
                        "this table's {why}; a write of that row waits for it"
                    )));
                }
                for row in &mut rows {
                    conform(table, row)?;
                }
                let mut applied = Applied::default();
                let mut appended = BTreeMap::new();
                let said = self.carried();
                for changes in self.compiled.packed(rows, said.as_deref())? {
                    let count = changes.len();
                    let request = if owner {
                        crate::crypto::random()?
                    } else {
                        derived_request(table, &changes, said.as_deref())?
                    };
                    let command = Command::Write {
                        request,
                        changes,
                        said: said.clone(),
                    };
                    self.compiled.admits(&command)?;
                    if !owner
                        && let Some(first) =
                            retry_or_appended(&log, session.actor(), &command, &appended)?
                    {
                        applied.unchanged += count;
                        applied.operations.push(first);
                        continue;
                    }
                    let receipt = application::append(session, store, &command.encode()?)?;
                    if !owner {
                        appended.insert(request, (receipt.id, command));
                    }
                    applied.records += 1;
                    applied.cells += count;
                    applied.operations.push(receipt.id);
                }
                Ok((planned, applied))
            },
        )
    }

    /// Write the cells a grid's text changed against the export `base` it was laid out from: the
    /// one save path of `aterm db table apply` and the Tables page. The text is read as the grid
    /// reads it ([`grid::plan`], a new row keyed by [`Compiled::born`]), then written by
    /// [`Self::apply`]. Cells the save cannot take, a refusal of the table's rules among them,
    /// come back as problems, and then nothing is written.
    ///
    /// # Errors
    /// As [`Self::view`] and [`Self::apply`].
    pub fn save(
        &self,
        collection: &'static Collection,
        base: &grid::Base,
        text: &str,
    ) -> Result<Saved> {
        let compiled = self.compiled;
        let view = self.view()?;
        let birth = |c: &Collection, values: &_| compiled.born(c, values, &view.cells);
        let plan = match grid::plan(compiled.table, collection, base, text, &birth) {
            Ok(plan) => plan,
            Err(found) => return Ok(Saved::Problems(found)),
        };
        let applied = self.apply(collection, &plan)?;
        if let Some(refused) = &applied.refused {
            return Ok(Saved::Problems(vec![edit::refused_problem(&plan, refused)]));
        }
        Ok(Saved::Written {
            plan: Box::new(plan),
            applied,
        })
    }

    /// Append `rows` to the log `collection` of this table as this Mac's entries, in as few
    /// records as a record's limits allow ([`pack`]: one, for up to 64 small rows), declaring
    /// the table first where this Mac's catalog does not yet: each at the key
    /// `<uuid32>.<n>`, this Mac's Commons machine uuid and its own counter, one past the highest
    /// it has spent in the log ([`owned::log_spent`]), in this generation or in a sealed one the
    /// family keeps ([`kept::Sealed::spent`]). The counter is read and the record
    /// appended under this Mac's Commons writer lock and the table's, which every writer of the
    /// Mac takes, so two agents on one Mac appending at once get two counters, in whichever order
    /// the lock admits them; two Macs never share a key, since the uuid is the signer's. No key
    /// is read off a clock: two entries stated in one millisecond are two entries. Returns each
    /// entry's key, in `rows`' order.
    ///
    /// A retry after a failure whose outcome is unknown appends again, at the next counter:
    /// each entry is a statement made once, and a log keeps every one.
    ///
    /// # Errors
    /// `collection` names no log of this table, or `rows` is empty; a row breaks its schema
    /// ([`owned::log_row`]) or passes a record's limits; no Commons writer, or this Mac is not
    /// in its own roster; this writer's history here is withheld or forked; this Mac does not
    /// hold a sealed generation the family keeps yet; or a record cannot be written.
    pub fn append_log(
        &self,
        collection: &Collection,
        rows: &[aterm_json::Map],
    ) -> Result<Vec<String>> {
        // The table's own declaration of the log, named as the caller names it: a constant's
        // address is no identity across crates.
        let Some(collection) = self
            .compiled
            .table
            .collection(collection.name)
            .filter(|held| held.kind == typed::Kind::Log && held.version == collection.version)
        else {
            return Err(Error::new(&format!(
                "{}: no log {} in this table",
                self.compiled.name(),
                collection.name
            )));
        };
        if rows.is_empty() {
            return Err(Error::new(&format!(
                "{}: nothing to append",
                collection.name
            )));
        }
        // Every row is checked before the lock is taken: a refused one writes nothing.
        for row in rows {
            owned::log_row(collection, row, 1)?;
        }
        // Every sealed generation of the family, whichever one this handle was asked for.
        let earlier = kept::before(self.family, self.compiled);
        application::with_store_until(
            &self.paths,
            true,
            self.descriptor(),
            crate::admission::deadline(),
            |session, store, scope| {
                session.writer()?;
                let decode = self.compiled.decoder();
                let log = Log::read_with(session, store, scope, decode)?;
                log.writable(session.actor())?;
                let me = session
                    .members()?
                    .iter()
                    .find(|binding| binding.actor() == session.actor())
                    .map(|binding| *binding.machine())
                    .ok_or_else(|| Error::new("this Mac is not in its own Commons roster"))?;
                let ops: Vec<Op> = log
                    .eligible_iter()
                    .filter_map(|(operation, command)| Op::from_eligible(0, operation, command))
                    .collect();
                let mut spent =
                    owned::log_spent(collection, &ops, owned::commons::historical(session), &me);
                // An entry of a sealed generation the family keeps stands at its key in every
                // later generation's view ([`kept::overlay`]), so the counter goes on past what
                // this Mac spent in each of them.
                for sealed in kept::sealed(session, &earlier)? {
                    spent = spent.max(sealed.spent(session, collection.name, &me)?);
                }
                let mut keys = Vec::with_capacity(rows.len());
                let mut changes = Vec::with_capacity(rows.len());
                for (i, row) in (1_u64..).zip(rows) {
                    let n = spent
                        .checked_add(i)
                        .ok_or_else(|| Error::new("this Mac's log counter is spent"))?;
                    #[cfg(test)]
                    let n = if log_seam::keys_by_clock() {
                        row.get("at").and_then(Value::as_u64).unwrap_or(n)
                    } else {
                        n
                    };
                    let key = owned::log_entry_key(&me, n);
                    changes.push(vec![Change {
                        collection: collection.name.to_owned(),
                        key: key.clone(),
                        value: Some(owned::log_row(collection, row, n)?),
                    }]);
                    keys.push(key);
                }
                let said = self.carried();
                for changes in pack_saying(changes, said.as_deref())? {
                    let command = Command::Write {
                        request: crate::crypto::random()?,
                        changes,
                        said: said.clone(),
                    };
                    let payload = command.encode()?;
                    // A record this build's own reader would refuse is never signed: it would
                    // read as refused history and stop this writer.
                    decode(&payload)?;
                    application::append(session, store, &payload)?;
                }
                Ok(keys)
            },
        )
    }

    /// Append the exact compensation of `operation` while every key it wrote still holds it as
    /// the sole candidate (`Log::compensate`); a later edit stays. The target, current value and
    /// preimage use the same history as [`Self::view`]: past-quota records and the writes the
    /// authority rules hold inert left out, so a revert never restores an inert write, and a key
    /// where the target's own write is inert is restated as it reads now. The compensation passes
    /// the table's rules as any owner write does ([`Compiled::rules`]), and is written exactly: a
    /// cell the rules would add is not. An exact retry of `request` returns the first operation.
    ///
    /// # Errors
    /// No Commons writer, the operation is not an eligible typed record here, a key it wrote
    /// changed since, or the table's rules refuse its compensation.
    pub fn revert(&self, request: [u8; 16], operation: Digest) -> Result<Digest> {
        let table = self.compiled.table;
        application::with_store(
            &self.paths,
            false,
            self.descriptor(),
            |session, store, scope| {
                session.writer()?;
                let log = Log::read_with(session, store, scope, self.compiled.decoder())?;
                log.writable(session.actor())?;
                if let Some(id) = log.revert_retry(session.actor(), request, operation)? {
                    return Ok(id);
                }
                let view = View::read(table, session, &log)?;
                let mut command =
                    log.compensate(request, operation, &view.quota.over, &view.inert.changes)?;
                if let Command::Revert { said, .. } = &mut command {
                    *said = self.carried();
                }
                self.compiled.admits(&command)?;
                if let Some(changes) = command.changes() {
                    refuse_secret_revert(table, changes)?;
                }
                if let Some(changes) = command.changes()
                    && let Some(refused) =
                        self.compiled
                            .rules(&mut [changes.to_vec()], &view, Author::Owner)?
                {
                    return Err(refused.into());
                }
                Ok(application::append(session, store, &command.encode()?)?.id)
            },
        )
    }

    /// Publish the compiled schema rows (design §3.5), an owner verb: only where the stored row
    /// is absent or below the compiled version, never automatically when a build opens the
    /// table, never below a version already visible and never a second row at an equal version.
    /// Returns the records written.
    ///
    /// A table whose secret cells name store entries ([`typed::Table::names_store`]) is refused:
    /// publishing would declare it, and this build writes no such table outside a test.
    ///
    /// # Errors
    /// No Commons writer, the table is one this build does not declare, or it cannot be read or
    /// written.
    pub fn publish_schema(&self) -> Result<usize> {
        let table = self.compiled.table;
        if table.names_store() {
            return Err(Error::new(
                "this build of aterm declares no table whose secret cells name secret-store \
                 entries; its schema is not published here",
            ));
        }
        application::with_store(
            &self.paths,
            true,
            self.descriptor(),
            |session, store, scope| {
                session.writer()?;
                let log = Log::read_with(session, store, scope, self.compiled.decoder())?;
                log.writable(session.actor())?;
                let mut written = 0;
                let mut appended = BTreeMap::new();
                let said = self.carried();
                let rows = schema_rows(table, &View::read(table, session, &log)?);
                for changes in pack_saying(rows, said.as_deref())? {
                    let request = derived_request(table, &changes, said.as_deref())?;
                    let command = Command::Write {
                        request,
                        changes,
                        said: said.clone(),
                    };
                    self.compiled.admits(&command)?;
                    if retry_or_appended(&log, session.actor(), &command, &appended)?.is_some() {
                        continue;
                    }
                    let receipt = application::append(session, store, &command.encode()?)?;
                    appended.insert(request, (receipt.id, command));
                    written += 1;
                }
                Ok(written)
            },
        )
    }
}

/// Refuse a revert whose compensation writes a secret's `SecretRef` cell (a secret-named column,
/// [`typed::secret_named`]; a `vault_ref` is an ordinary reference): the write it undoes removed the
/// entry the preimage names once it landed (`alab::secret::settle_write`) and put its own value
/// into an entry nothing else owes, so restoring the old reference would name a value that is
/// gone and leave the newer one in the store with no cell naming it. The owner writes the value
/// again instead, through the verb that owns the store side.
///
/// # Errors
/// A change names a `SecretRef` cell; the refusal names its column, never a value.
fn refuse_secret_revert(table: &typed::Table, changes: &[Change]) -> Result<()> {
    for change in changes {
        let Ok(key) = typed::cell::CellKey::parse(&change.key) else {
            continue;
        };
        if table
            .collection(&change.collection)
            .and_then(|collection| collection.column(key.col))
            .is_some_and(|column| {
                column.ty == typed::Ty::SecretRef && typed::secret_named(column.name)
            })
        {
            return Err(Error::new(&format!(
                "a revert never restores a secret cell ({}.{}): the write it would undo removed \
                 the store entry the old value names; write the value again with the table's \
                 own `set <row> {} --stdin`",
                change.collection, key.col, key.col
            )));
        }
    }
    Ok(())
}

/// Check whether `command` is an exact retry of an eligible record in `log` or of a record already
/// appended in the current write batch (`appended`), without re-reading the store log on every
/// append.
/// The request id of a record whose id derives from its content ([`typed::request_id`]): its
/// changes', and where it says who made it ([`crate::said`]), that statement's too, so a retry
/// of the same statement returns its first receipt and another principal's identical changes
/// are that principal's own record.
///
/// # Errors
/// The changes or the statement cannot be encoded.
pub(crate) fn derived_request(
    table: &Table,
    changes: &[Change],
    said: Option<&Said>,
) -> Result<[u8; 16]> {
    said_request(
        typed::request_id(table.application, table.role, changes)?,
        said,
    )
}

/// `request`, a content-derived request id, as the record of who `said` it carries it
/// ([`derived_request`]): itself where the record states nothing.
///
/// # Errors
/// The statement cannot be encoded.
pub(crate) fn said_request(request: [u8; 16], said: Option<&Said>) -> Result<[u8; 16]> {
    let Some(said) = said else {
        return Ok(request);
    };
    let statement = aterm_json::to_vec(&said.to_value())
        .map_err(|_| Error::new("a statement of who acted could not be encoded"))?;
    let mut sha = aterm_digest::Sha256::new();
    sha.update(b"aterm/typed/said-request/1\0");
    sha.update(request);
    sha.update(&statement);
    let digest = sha.finalize();
    let mut out = [0; 16];
    out.copy_from_slice(&digest[..16]);
    Ok(out)
}

pub(crate) fn retry_or_appended(
    log: &Log,
    actor: Actor,
    command: &Command,
    appended: &BTreeMap<[u8; 16], (Digest, Command)>,
) -> Result<Option<Digest>> {
    if let Some(first) = log.retry(actor, command)? {
        return Ok(Some(first));
    }
    if let Some((first, prev)) = appended.get(&command.request()) {
        if prev != command {
            return Err(Error::new(
                "request identity reused with different commands",
            ));
        }
        return Ok(Some(*first));
    }
    Ok(None)
}

/// The schema rows a build compiled at these versions may publish over `view`: a collection
/// whose stored row is absent or below the compiled version. Never one below a version already
/// visible, and never a second row at an equal version (that would split it).
pub(crate) fn schema_rows(table: &Table, view: &View) -> Vec<Vec<Change>> {
    table
        .collections
        .iter()
        .filter(|collection| match view.schema(collection.name) {
            SchemaState::Absent => true,
            SchemaState::One { version, .. } | SchemaState::Split { version } => {
                version < collection.version
            }
        })
        .map(|collection| {
            vec![Change {
                collection: typed::SCHEMA.to_owned(),
                key: collection.name.to_owned(),
                value: Some(collection.schema_row()),
            }]
        })
        .collect()
}

/// Read a regular UTF-8 file of at most `limit` bytes without following a final symlink or
/// blocking on a FIFO (`LOCK_OPEN_FLAGS`).
pub(crate) fn read_regular_utf8(
    path: &std::path::Path,
    limit: usize,
    unreadable: &str,
    oversize: &str,
    non_utf8: &str,
) -> Result<String> {
    use std::io::Read as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let err = || Error::new(unreadable);
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS)
        .open(path)
        .map_err(|_| err())?;
    let meta = file.metadata().map_err(|_| err())?;
    if !meta.is_file() {
        return Err(err());
    }
    if meta.len() > limit as u64 {
        return Err(Error::new(oversize));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len().saturating_add(1)).unwrap_or(0));
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| err())?;
    if bytes.len() > limit {
        return Err(Error::new(oversize));
    }
    String::from_utf8(bytes).map_err(|_| Error::new(non_utf8))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) mod kept_tests;
#[cfg(test)]
pub(crate) mod said_tests;

/// The log keys a clock would make: the negative control of the exit test of `lab/tracker`'s
/// logs (`file::tests`).
#[cfg(test)]
#[path = "att_log_seam_tests.rs"]
pub(crate) mod log_seam;
