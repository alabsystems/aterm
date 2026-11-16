// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A table's history in pages (docs/DESIGN-tables-app-2026-11-14.md §4.1–§4.3).
//!
//! A page is one store read. The causal clocks are folded over the whole authenticated index,
//! which the order needs, and the quota over every eligible record, which a record's inertness
//! needs; both judge each record in place. The filters and the cursor are then applied, and only
//! the records the page keeps are copied out as [`Entry`]s, their writers' machines resolved.
//! The cursor is the sort key, (causal clock, operation digest). A page carries the
//! [`Revision`] it was read at; a page asked at another revision says so ([`Page::moved`]) and
//! starts from the top, since an operation arriving late with an earlier clock, or a change of
//! membership, can land inside a range already served. No fold is kept between calls: a reader
//! asks [`Handle::revision`], which opens no payload, to learn whether to read again.

use super::{Handle, decode};
use crate::{
    Error, Result,
    database::{Change, Command, decode_hex},
    documents::{application, log::Log},
    typed::{
        self, Table,
        cell::{CellKey, CellValue},
        owned::{self},
    },
};
use astream_wire::sync::{Actor, Digest};
use aterm_codec::hex;

use crate::documents::log::Hlc;

/// What kind of write a record is, decided here once for every reader: `aterm` where the
/// store's own rule calls it automatic ([`owned::automatic`]: a write to an owned or log
/// collection, or a cell marked `a`), `import` where a cell carries an import source `s`, else
/// `explicit`. A revert is explicit whatever value it restores. A record that is several of
/// these is the first that holds, in that order. The record does not say who typed an explicit
/// write: a person and a program at the same Mac write the same record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EntryKind {
    /// Written by aterm itself: a birth row, a seed, a Mac's own owned or log rows.
    Aterm,
    /// Written by an import from a source.
    Import,
    /// Written on purpose through a verb, the grid or a revert, by a person or a program.
    Explicit,
}

impl EntryKind {
    /// The kind of a record that `revert`s or not and writes `changes` in `table`.
    pub fn of(table: &Table, revert: bool, changes: &[Change]) -> Self {
        if revert {
            Self::Explicit
        } else if owned::automatic_record(table, revert, changes) {
            Self::Aterm
        } else if changes.iter().any(|change| {
            table
                .collection(&change.collection)
                .is_some_and(|collection| collection.kind == typed::Kind::Cells)
                && change
                    .value
                    .as_ref()
                    .and_then(|value| CellValue::decode(value).ok())
                    .is_some_and(|cell| cell.s.is_some())
        }) {
            Self::Import
        } else {
            Self::Explicit
        }
    }

    /// The word history prints.
    pub const fn word(self) -> &'static str {
        match self {
            Self::Aterm => "aterm",
            Self::Import => "import",
            Self::Explicit => "explicit",
        }
    }

    /// The kind [`Self::word`] spells.
    pub fn parse(word: &str) -> Option<Self> {
        [Self::Aterm, Self::Import, Self::Explicit]
            .into_iter()
            .find(|kind| kind.word() == word)
    }
}

/// Why a record in history projects nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Inert {
    /// An automatic record past its writer's quota ([`owned::quota`]): it stays in history and
    /// in every causal past, and never projects.
    Quota,
    /// Every row it writes is an owned or log row no reader keeps: a key that is not its
    /// signer's machine, a body without a counter or past the row bound, or a removal.
    Rows,
}

impl Inert {
    /// The word history prints.
    pub const fn word(self) -> &'static str {
        match self {
            Self::Quota => "quota",
            Self::Rows => "rows",
        }
    }
}

/// One eligible operation as history shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub operation: Digest,
    pub actor: Actor,
    /// The writer's Commons machine, from the binding in the roster the record names.
    pub machine: Option<[u8; 16]>,
    /// The signed writer clock claim, retained separately from the derived causal order.
    pub authored_ms: u64,
    /// Read-time clock derived from the authenticated causal graph, including hidden parents.
    pub hlc: Hlc,
    pub request: Option<[u8; 16]>,
    pub reverts: Option<Digest>,
    /// What kind of write it is ([`EntryKind::of`]).
    pub kind: EntryKind,
    /// Why it projects nothing, where it does not.
    pub inert: Option<Inert>,
    pub changes: Vec<Change>,
}

impl Entry {
    /// Where a page that ends at this entry goes on from.
    pub const fn cursor(&self) -> Cursor {
        Cursor {
            hlc: self.hlc,
            operation: self.operation,
        }
    }
}

/// A position in history: the sort key of the last entry a page served. The next page holds
/// the entries after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Cursor {
    pub hlc: Hlc,
    pub operation: Digest,
}

impl Cursor {
    /// `<ms>.<counter>.<operation hex>`, as `aterm db table history` prints it.
    pub fn text(&self) -> String {
        format!(
            "{}.{}.{}",
            self.hlc.ms,
            self.hlc.counter,
            hex::encode_lower(&self.operation)
        )
    }

    /// The cursor [`Self::text`] spells.
    ///
    /// # Errors
    /// `text` is not `<ms>.<counter>.<64 hex>`.
    pub fn parse(text: &str) -> Result<Self> {
        let bad =
            || Error::new("a history cursor is <ms>.<counter>.<operation>, as history prints it");
        let mut parts = text.splitn(3, '.');
        let ms = parts.next().and_then(|p| p.parse().ok()).ok_or_else(bad)?;
        let counter = parts.next().and_then(|p| p.parse().ok()).ok_or_else(bad)?;
        let operation = parts
            .next()
            .and_then(|p| decode_hex::<32>(p).ok())
            .ok_or_else(bad)?;
        Ok(Self {
            hlc: Hlc { ms, counter },
            operation,
        })
    }
}

/// What this Mac holds of a table at one moment ([`crate::table::store::Store::revision`]): two
/// reads at one revision see the same eligible records in the same order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Revision(pub Digest);

impl Revision {
    /// Its lower-case hex.
    pub fn text(&self) -> String {
        hex::encode_lower(&self.0)
    }

    /// The revision [`Self::text`] spells.
    ///
    /// # Errors
    /// `text` is not 64 hex digits.
    pub fn parse(text: &str) -> Result<Self> {
        decode_hex::<32>(text)
            .map(Self)
            .map_err(|_| Error::new("a history revision is 64 hex digits, as history prints it"))
    }
}

/// Whose records a page keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Writer {
    /// One Commons writer.
    Actor(Actor),
    /// Every writer the roster each record names binds to this Commons machine.
    Machine([u8; 16]),
}

/// Which records a page keeps. A record is kept where its writer and kind match and, where a
/// collection, row or column is named, one of its changes is to that place.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filter {
    pub collection: Option<String>,
    /// A row's key: a change at `<row>` or `<row>.…`.
    pub row: Option<String>,
    /// A typed cell's column ([`CellKey`]).
    pub column: Option<String>,
    pub writer: Option<Writer>,
    pub kind: Option<EntryKind>,
}

impl Filter {
    /// Whether `change` is to the place this filter names (any change, where it names none).
    pub fn wants(&self, change: &Change) -> bool {
        self.collection
            .as_deref()
            .is_none_or(|name| change.collection == name)
            && self.row.as_deref().is_none_or(|row| {
                change.key == row
                    || change
                        .key
                        .strip_prefix(row)
                        .is_some_and(|rest| rest.starts_with('.'))
            })
            && self
                .column
                .as_deref()
                .is_none_or(|column| CellKey::parse(&change.key).is_ok_and(|key| key.col == column))
    }
}

/// One page asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ask {
    pub filter: Filter,
    /// The entries after this position; from the top where `None`.
    pub after: Option<Cursor>,
    /// At most this many entries.
    pub limit: usize,
    /// The revision the reader's earlier pages were read at; where the table is at another now,
    /// the page says so and starts from the top.
    pub revision: Option<Revision>,
}

impl Ask {
    /// The first `limit` entries `filter` keeps.
    pub const fn first(filter: Filter, limit: usize) -> Self {
        Self {
            filter,
            after: None,
            limit,
            revision: None,
        }
    }

    /// The page after `page`, `limit` entries long; `None` once `page` was the last.
    pub fn after(&self, page: &Page) -> Option<Self> {
        Some(Self {
            filter: self.filter.clone(),
            after: Some(page.next?),
            limit: self.limit,
            revision: Some(page.revision),
        })
    }
}

/// One page of history.
#[derive(Clone, Debug, PartialEq)]
pub struct Page {
    /// The revision the page was read at.
    pub revision: Revision,
    /// It was asked at another revision, so it starts from the top and the reader's earlier
    /// pages are stale.
    pub moved: bool,
    /// The entries, in history's order.
    pub entries: Vec<Entry>,
    /// Where the next page starts; `None` where no entry the filter keeps follows.
    pub next: Option<Cursor>,
}

impl Handle {
    /// One page of this table's eligible history ([`Ask`]), in derived causal-clock order with
    /// concurrent edits ordered by clock then digest, read in one store read.
    ///
    /// # Errors
    /// As [`Self::view`]; also refuses incomplete causal clocks, a logical counter overflow, or
    /// a page of no entries.
    pub fn history_page(&self, ask: &Ask) -> Result<Page> {
        if ask.limit == 0 {
            return Err(Error::new("a history page holds at least one entry"));
        }
        let table = self.compiled.table;
        super::reopened(crate::admission::deadline(), || {
            application::observe_store(&self.paths, self.descriptor(), |session, store, scope| {
                let revision = Revision(store.revision(session)?);
                let log = Log::read_with(session, store, scope, decode)?;
                let resolve = owned::commons::historical(session);
                page(table, &log, revision, ask, &resolve)
            })
        })
    }

    /// Every eligible edit, as one page of every entry ([`Self::history_page`]).
    ///
    /// # Errors
    /// As [`Self::history_page`].
    pub fn history(&self) -> Result<Vec<Entry>> {
        self.history_page(&Ask::first(Filter::default(), usize::MAX))
            .map(|page| page.entries)
    }

    /// The revision this Mac holds the table at, read without opening a payload: a reader that
    /// read a page or a view at this revision has nothing new to read.
    ///
    /// # Errors
    /// As [`Self::view`].
    pub fn revision(&self) -> Result<Revision> {
        super::reopened(crate::admission::deadline(), || {
            application::observe_store(&self.paths, self.descriptor(), |session, store, _| {
                store.revision(session).map(Revision)
            })
        })
    }
}

/// The page `ask` names of `log`, which was read at `revision`; `resolve` finds a record's
/// writer machine through the roster it names.
fn page(
    table: &'static Table,
    log: &Log,
    revision: Revision,
    ask: &Ask,
    resolve: &dyn Fn(&Digest, &Actor) -> Option<[u8; 16]>,
) -> Result<Page> {
    let order = log.causal_order()?;
    let (records, merges) = log.eligible_and_merges();
    let quota = owned::quota_of(
        table,
        records.iter().filter_map(|(operation, command)| {
            let changes = command.changes()?;
            Some((
                operation.header.actor,
                operation.header.sequence,
                operation.header.id,
                owned::automatic_record(table, is_revert(command), changes),
            ))
        }),
        &merges,
    );
    let moved = ask.revision.is_some_and(|asked| asked != revision);
    let start = match ask.after.filter(|_| !moved) {
        Some(after) => {
            order.partition_point(|&(hlc, id)| (hlc, id) <= (after.hlc, after.operation))
        }
        None => 0,
    };
    let filter = &ask.filter;
    let mut entries = Vec::new();
    let mut next = None;
    for &(hlc, id) in &order[start..] {
        let Some((operation, command)) = log.eligible_record(&id) else {
            continue;
        };
        let Some(changes) = command.changes() else {
            continue;
        };
        let revert = is_revert(command);
        let kind = EntryKind::of(table, revert, changes);
        if filter.kind.is_some_and(|wanted| wanted != kind) {
            continue;
        }
        let actor = operation.header.actor;
        let mut machine = None;
        let mut signer = || *machine.get_or_insert_with(|| resolve(&operation.roster, &actor));
        let writes = match filter.writer {
            None => true,
            Some(Writer::Actor(wanted)) => wanted == actor,
            Some(Writer::Machine(wanted)) => signer() == Some(wanted),
        };
        if !writes
            || (filter.collection.is_some() || filter.row.is_some() || filter.column.is_some())
                && !changes.iter().any(|change| filter.wants(change))
        {
            continue;
        }
        if entries.len() == ask.limit {
            next = entries.last().map(Entry::cursor);
            break;
        }
        let machine = signer();
        let inert = if quota.over.contains(&id) {
            Some(Inert::Quota)
        } else if changes.iter().all(|change| {
            table
                .collection(&change.collection)
                .is_some_and(|collection| collection.kind != typed::Kind::Cells)
                && !owned::kept(table, change, machine)
        }) {
            Some(Inert::Rows)
        } else {
            None
        };
        entries.push(Entry {
            operation: id,
            actor,
            machine,
            authored_ms: operation.authored_ms,
            hlc,
            request: operation.request,
            reverts: operation.reverts,
            kind,
            inert,
            changes: changes.to_vec(),
        });
    }
    Ok(Page {
        revision,
        moved,
        entries,
        next,
    })
}

const fn is_revert(command: &Command) -> bool {
    matches!(command, Command::Revert { .. })
}
