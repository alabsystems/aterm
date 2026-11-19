// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A table's history in pages (docs/DESIGN-tables-app-2026-11-14.md §4.1–§4.3).
//!
//! A page is cut from the whole of one revision's history, ordered and resolved ([`Kept`]). That
//! is read once a revision: the causal clocks are folded over the whole authenticated index,
//! which the order needs, and the quota over every eligible record, which a record's inertness
//! needs; every eligible record is then copied out as an [`Entry`], its writer's machine
//! resolved and named by the Mac its facts bind ([`Mac`], design §4.5), from the table as the
//! same read projects it. A page applies the filters and the cursor to that order.
//! The cursor is the sort key, (causal clock, operation digest); a page read newest first runs
//! back from history's end, its cursor naming the entry it goes on before. A page carries the
//! [`Revision`] it was read at; a page asked at another revision says so ([`Page::moved`]) and
//! starts from the top, since an operation arriving late with an earlier clock, or a change of
//! membership, can land inside a range already served.
//!
//! A reader that asks page after page keeps a [`Kept`] between them
//! ([`Handle::history_page_kept`]): each page reads the store's revision, which opens no payload,
//! and where the kept order is of that revision the page is cut from it, decoding, projecting
//! and ordering nothing; at any other revision the store is read again and the order replaced.
//! The revision is read in the same observation as the page, so a page is never cut from an
//! order the table has moved past. Beside the order, the reader keeps the sighting of the store
//! its last open read ([`crate::table::store::Sighting`], tracker LT-25C): where the order is of
//! that sighting's revision and the store stands as the sighting's open read it, the page's
//! observation learns the revision unchanged without opening the store, authenticating no
//! record; anywhere else the store is opened, as every read of it opens it.
//!
//! The same kept sighting answers a reader that asks only whether the table moved
//! ([`Handle::revision_kept`], tracker LT-25D: the Tables page's change check): while the store
//! stands as the sighting's open read it, the revision is the sighting's and no store is opened;
//! anywhere else the store is opened, its answer stands and its sighting is the one kept. That
//! read builds no order, so a reader that asks no page keeps the sighting alone; and since a page
//! and a revision read keep one sighting between them, each is answered from the other's open
//! while the store stands still.

use super::{Handle, Usage, View};
use crate::said::Said;
use crate::{
    Error, Result,
    alab::registry::{MACHINE, MACHINE_FACTS},
    catalog::Sighted,
    database::{Change, Command, decode_hex},
    documents::{application, log::Log},
    typed::{
        self, Table,
        cell::{CellKey, CellValue, Projection},
        claim::Settlements,
        keys,
        owned::{self, OwnedRead},
    },
};
use astream_wire::sync::{Actor, Digest};
use aterm_codec::hex;
use aterm_secrets::client::Paths;
use std::collections::BTreeMap;

use crate::documents::log::Hlc;

/// What kind of write a record is, decided here once for every reader: `aterm` where the
/// store's own rule calls it automatic ([`owned::automatic`]: a write to an owned or log
/// collection, or a cell marked `a`), `import` where a cell carries an import source `s`, else
/// `explicit`. A revert is explicit whatever value it restores. A record that is several of
/// these is the first that holds, in that order. A version 1 record does not say who typed an
/// explicit write: a person and a program at the same Mac write the same record. A version 2
/// record says it ([`Entry::said`]), in a generation that takes version 2 records.
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
    /// Which generation of its family the record is of: the one whose log holds it, under its
    /// own signer's signature. A family that keeps its records across a roll lists the entries
    /// of its sealed generations beside its current one's ([`super::kept`]).
    pub generation: u32,
    pub actor: Actor,
    /// The writer's Commons machine, from the binding in the roster the record names.
    pub machine: Option<[u8; 16]>,
    /// The Mac history names the writer by ([`Macs::name`]).
    pub mac: Mac,
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
    /// What a version 2 record says of who made it, why and on what evidence
    /// ([`crate::said`]); `None` for a version 1 record, which names only its signing Mac.
    pub said: Option<Said>,
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

/// The Mac history names a writer by (design §4.5). Labels do not reliably name a Mac
/// (`docs/KNOWN_ISSUES.md` ATERM-KI-1112-155), so the name is the machine row the writer's own
/// facts bind, never a row found by its label.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Mac {
    /// The `machine` row the writer's `machine-facts` row binds ([`crate::alab::registry::bound_machine`]:
    /// its `merged_into`, else its `mac`).
    Row {
        key: String,
        /// The row's label, where it holds one and no conflict.
        label: Option<String>,
        /// The end of the row's key, where another row holds the same label: the shortest of
        /// four or more characters no other such row's key ends with.
        ending: Option<String>,
    },
    /// No facts row binds the writer's Commons machine.
    Machine([u8; 16]),
    /// The roster the record names binds its writer to no Commons machine.
    Actor(Actor),
}

impl Mac {
    /// How a person reads it: the row's label (`m73`), with its key's ending where another row
    /// shares the label (`m73 (…5c4f)`), else the row's key; a Commons machine id where no row
    /// binds; the writer's actor where no machine does.
    pub fn name(&self) -> String {
        match self {
            Self::Row {
                label: Some(label),
                ending: Some(ending),
                ..
            } => format!("{label} (\u{2026}{ending})"),
            Self::Row {
                label: Some(label), ..
            } => label.clone(),
            Self::Row { key, .. } => key.clone(),
            Self::Machine(machine) => keys::uuid32(machine),
            Self::Actor(actor) => hex::encode_lower(actor),
        }
    }

    /// The machine row's key, where one binds.
    pub fn row(&self) -> Option<&str> {
        match self {
            Self::Row { key, .. } => Some(key),
            Self::Machine(_) | Self::Actor(_) => None,
        }
    }
}

/// One observation of a table's head and ordered history.
#[derive(Debug)]
pub struct HistoryView {
    /// The current generation's head.
    pub view: View,
    /// Storage usage of the current generation.
    pub usage: Usage,
    /// The eligible edits in causal order, including sealed generations whose records are kept.
    pub entries: Vec<Entry>,
    /// The head's machine bindings and labels used to name every entry's writer.
    pub macs: Macs,
    /// The head's machine rows and claim settlements ([`View::project_settled`]), using the
    /// generation's machine schema. These are the rows that supplied [`Self::macs`].
    pub machines: (Projection, Settlements),
}

/// Which machine row each Commons machine's facts bind, and what each such row is called: the
/// join from a writer to its Mac, read from one projection of the table.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Macs {
    /// The machine row each Commons machine's facts row binds; a predecessor (writer
    /// succession) whose own facts row binds none takes its successor's.
    bound: BTreeMap<[u8; 16], String>,
    /// Each machine row's unconflicted label.
    labels: BTreeMap<String, String>,
    /// The key ending of each row whose label another row holds too.
    endings: BTreeMap<String, String>,
}

impl Macs {
    /// The join `view` holds: its `machine-facts` rows and its `machine` rows.
    pub fn of(view: &View) -> Self {
        Self::from_projected(view, &view.project(&MACHINE))
    }

    fn from_projected(view: &View, machines: &Projection) -> Self {
        let empty = OwnedRead::default();
        Self::from_rows(
            machines,
            view.owned.get(MACHINE_FACTS.name).unwrap_or(&empty),
        )
    }

    /// The join `facts` (each Mac's own statement, keyed by its Commons machine) and `machines`
    /// (the registry's machine rows) hold: each writer's machine row as its facts bind it, a
    /// predecessor's through its successor ([`crate::alab::link::bound`]).
    pub fn from_rows(machines: &Projection, facts: &OwnedRead) -> Self {
        let bound = crate::alab::link::bound(facts);
        let labels: BTreeMap<String, String> = machines
            .rows
            .iter()
            .filter_map(|(key, row)| {
                if row.raw.contains("label")
                    || row
                        .fields
                        .get("label")
                        .is_some_and(|field| field.conflicted())
                {
                    return None;
                }
                let label = row
                    .get("label")?
                    .as_str()
                    .filter(|label| !label.is_empty())?;
                Some((key.clone(), label.to_owned()))
            })
            .collect();
        let mut sharing: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for (key, label) in &labels {
            sharing.entry(label).or_default().push(key);
        }
        let mut endings = BTreeMap::new();
        for group in sharing.values().filter(|group| group.len() > 1) {
            for key in group {
                endings.insert((*key).to_owned(), ending(key, group));
            }
        }
        Self {
            bound,
            labels,
            endings,
        }
    }

    /// The Mac of the writer that signed as `actor` from Commons machine `machine`.
    pub fn name(&self, actor: &Actor, machine: Option<&[u8; 16]>) -> Mac {
        let Some(machine) = machine else {
            return Mac::Actor(*actor);
        };
        match self.bound.get(machine) {
            Some(key) => Mac::Row {
                key: key.clone(),
                label: self.labels.get(key).cloned(),
                ending: self.endings.get(key).cloned(),
            },
            None => Mac::Machine(*machine),
        }
    }
}

/// The shortest end of `key`, four characters or more, that no other key of `group` ends with;
/// the whole key where every end is another's.
fn ending(key: &str, group: &[&str]) -> String {
    let starts: Vec<usize> = key.char_indices().map(|(at, _)| at).rev().collect();
    starts
        .iter()
        .skip(3)
        .map(|&at| &key[at..])
        .find(|end| {
            group
                .iter()
                .all(|other| *other == key || !other.ends_with(end))
        })
        .unwrap_or(key)
        .to_owned()
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

    /// Whether a page keeps `entry`: its kind and writer match, and, where a place is named, one
    /// of its changes is to that place.
    fn keeps(&self, entry: &Entry) -> bool {
        self.kind.is_none_or(|wanted| wanted == entry.kind)
            && match self.writer {
                None => true,
                Some(Writer::Actor(wanted)) => wanted == entry.actor,
                Some(Writer::Machine(wanted)) => entry.machine == Some(wanted),
            }
            && (self.collection.is_none() && self.row.is_none() && self.column.is_none()
                || entry.changes.iter().any(|change| self.wants(change)))
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
    /// Newest first: the page runs back through history from its end, and `after` names the
    /// entry the entries before which it holds. A reader that lists the newest record at its top
    /// (the Tables page's History) is served its first page in one call, where oldest first it
    /// would ask page after page to reach the end.
    pub newest_first: bool,
}

impl Ask {
    /// The first `limit` entries `filter` keeps.
    pub const fn first(filter: Filter, limit: usize) -> Self {
        Self {
            filter,
            after: None,
            limit,
            revision: None,
            newest_first: false,
        }
    }

    /// The newest `limit` entries `filter` keeps, newest first.
    pub const fn newest(filter: Filter, limit: usize) -> Self {
        Self {
            filter,
            after: None,
            limit,
            revision: None,
            newest_first: true,
        }
    }

    /// The page after `page`, `limit` entries long; `None` once `page` was the last.
    pub fn after(&self, page: &Page) -> Option<Self> {
        Some(Self {
            filter: self.filter.clone(),
            after: Some(page.next?),
            limit: self.limit,
            revision: Some(page.revision),
            newest_first: self.newest_first,
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
    /// The entries, in history's order; newest first where they were asked so.
    pub entries: Vec<Entry>,
    /// Where the next page starts; `None` where no entry the filter keeps follows.
    pub next: Option<Cursor>,
    /// The records the table holds and does not project, every one whatever the filter keeps:
    /// what [`crate::typed::refused_words`] says of them.
    pub refused: Vec<crate::typed::Unread>,
}

/// The whole of one revision of one table's history, ordered and resolved: what every page at
/// that revision is cut from ([`Handle::history_page_kept`]). It holds one revision of one
/// table at a time; a page at any other revision, or of another table or home, replaces it.
/// Empty until a page fills it, and its order goes when it is dropped.
///
/// Beside the order it keeps the sighting of the store the last open made with it read, by a
/// page or by a revision read ([`Handle::revision_kept`]), with the home and the table that
/// store is of: what either read asks, of that home and table alone, to learn the store
/// unchanged without opening it. The home is the one that open read, which a handle whose
/// writer was succeeded names by its predecessor: the sighting is of the selected home, never
/// of the path the handle was made at. Where it holds both of one home and table, the order is
/// of the sighting's revision: a revision read that opens the store at another lets the order
/// go, and a page that reads another replaces both. A reader that asks no page keeps the sighting
/// alone, which holds the store's claim, floor and permit files' bytes, its journal prefix's
/// length and SHA-256 and each record class's verdict, and no record.
///
/// That one revision is every eligible entry of the table's history, those below a final
/// checkpoint's floor included (the store's quotas bound only the records above it), so its
/// memory grows with the table's history: 50.8 MiB at 105,043 entries of one small change each,
/// more where an entry's changes and its writer's Mac are wider. Its reader holds it for as
/// long as it reads.
#[derive(Default)]
pub struct Kept {
    /// The store as the last open made with this read it, where that open made a sighting.
    seen: Option<Seen>,
    order: Option<Order>,
    reads: u64,
}

impl Kept {
    /// The revision the kept order is of, where one is kept.
    pub fn revision(&self) -> Option<Revision> {
        self.order.as_ref().map(|order| order.revision)
    }

    /// The revision the kept sighting's open read, where a sighting is kept: what a revision
    /// read ([`Handle::revision_kept`]) answers while the store stands as that open read it.
    pub fn sighted(&self) -> Option<Revision> {
        self.seen.as_ref().map(Seen::revision)
    }

    /// How many times a page asked with this read its table and kept the order it read: once a
    /// revision, while its pages are asked at the revision it holds. A reader counts these to
    /// learn that a page cut from the kept order read nothing again.
    pub const fn reads(&self) -> u64 {
        self.reads
    }
}

impl std::fmt::Debug for Kept {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Kept")
            .field("revision", &self.revision())
            .field("sighted", &self.sighted())
            .field(
                "entries",
                &self.order.as_ref().map(|order| order.entries.len()),
            )
            .field("reads", &self.reads)
            .finish()
    }
}

/// [`Kept`]'s order: whose table it is, at which revision, and every entry.
struct Order {
    paths: Paths,
    table: &'static super::Compiled,
    revision: Revision,
    /// Every eligible entry, in history's order (oldest first).
    entries: Vec<Entry>,
    /// The records the read held and does not project ([`Page::refused`]).
    refused: Vec<crate::typed::Unread>,
}

impl Order {
    /// Whether this is the order of `table` at `paths` at `revision`. The revision digests the
    /// table's scope and this Mac's access with every record, so it alone names the order; the
    /// home and the table are compared too ([`same`]), since a [`Kept`] may be handed any handle.
    fn of(&self, paths: &Paths, table: &super::Compiled, revision: Revision) -> bool {
        self.revision == revision && same(self.table, table) && self.paths == *paths
    }
}

/// [`Kept`]'s sighting: the store of one table at one home as the last open made with the
/// [`Kept`] read it ([`crate::table::store::Store::sighted_revision`]). A read asks it only of
/// that home and table ([`Self::is_of`]), and a read that finds the store standing as it read it
/// learns its revision unchanged without opening the store.
///
/// The home is the one whose store was opened: the home the read selected
/// ([`crate::table::Session::paths`]), which is the handle's own until its writer is succeeded
/// and the successor's from then (`generation::observed_generation_until`). A handle's path
/// names two homes across a succession, so it does not say which store a sighting is of.
struct Seen {
    home: Paths,
    table: &'static super::Compiled,
    sighting: crate::table::store::Sighting,
}

impl Seen {
    /// The sighting of the store of `table` at `home`, the home a read selected, that the
    /// read's open there made.
    fn of(
        home: &Paths,
        table: &'static super::Compiled,
        sighting: Option<crate::table::store::Sighting>,
    ) -> Option<Self> {
        sighting.map(|sighting| Self {
            home: home.clone(),
            table,
            sighting,
        })
    }

    /// The revision the sighting's open read: the one the sighting itself holds, never a copy
    /// kept beside it.
    fn revision(&self) -> Revision {
        Revision(self.sighting.revision())
    }

    /// Whether this is a sighting of the store of `table` at `home`, the home a read selected:
    /// the one a read of that table there may ask after. A sighting of any other is never
    /// asked, the store a succeeded handle read before its succession included: its store's
    /// claim would differ and refuse it ([`crate::table::store::Store::sighting_stands`]), but a
    /// read does not lean on that.
    fn is_of(&self, home: &Paths, table: &super::Compiled) -> bool {
        same(self.table, table) && self.home == *home
    }
}

/// Whether `kept` and `table` are one table, compared by what names its store (application,
/// role and descriptor version) and its generation, never by address: a compiled table is a
/// `const` (`att::REGISTRY`), and each `&REGISTRY` may be a copy of its own, so a handle made at
/// one place and a handle made at another are of the same table at different addresses.
fn same(kept: &super::Compiled, table: &super::Compiled) -> bool {
    kept.table.application == table.table.application
        && kept.table.role == table.table.role
        && kept.version == table.version
        && kept.generation == table.generation
}

impl Handle {
    /// One page of this table's eligible history ([`Ask`]), in derived causal-clock order with
    /// concurrent edits ordered by clock then digest (newest first where [`Ask::newest_first`]),
    /// read in one store read; in a family that keeps its records across a roll, one read of each
    /// generation's store ([`super::kept`]). A reader that asks more than one page keeps their
    /// order between them ([`Self::history_page_kept`]).
    ///
    /// # Errors
    /// As [`Self::view`]; also refuses incomplete causal clocks, a logical counter overflow, or
    /// a page of no entries; and, for a family that keeps its records, where this Mac does not
    /// hold a sealed generation yet.
    pub fn history_page(&self, ask: &Ask) -> Result<Page> {
        self.read_history_page(ask, None)
    }

    /// [`Self::history_page`] cut from `kept` where it holds this table's order at the revision
    /// the store is at, which is read without opening a payload: such a page decodes, projects
    /// and orders no record. Else the store is read in the same observation, and `kept` takes
    /// the order of that revision once the observation stands, its earlier order let go first,
    /// so it never holds two. A page is never cut from an order of any revision but the one its
    /// own observation read. Where `kept` holds the order of its sighting's revision, the
    /// observation asks that sighting first and opens no store while it stands; a page that
    /// opens the store leaves `kept` the sighting of that open, which a revision read
    /// ([`Self::revision_kept`]) asks in its turn.
    ///
    /// # Errors
    /// As [`Self::history_page`]. On an error `kept` holds no order or the one it held, never a
    /// part of one, and the sighting it held (none for a family read across a roll, whose page
    /// asks and keeps no sighting).
    pub fn history_page_kept(&self, ask: &Ask, kept: &mut Kept) -> Result<Page> {
        self.read_history_page(ask, Some(kept))
    }

    /// [`Self::history_page_kept`] where `kept` is given; with none, the page takes its entries
    /// out of the order it read rather than copying them, so a read of the whole history
    /// ([`Self::history`]) holds one copy of it, not two.
    fn read_history_page(&self, ask: &Ask, mut kept: Option<&mut Kept>) -> Result<Page> {
        if ask.limit == 0 {
            return Err(Error::new("a history page holds at least one entry"));
        }
        let compiled = self.compiled;
        let table = compiled.table;
        // The sealed generations a family that keeps its records lists beside this one
        // ([`super::kept`]). A sighting is of one store, so a page that reads more than one asks
        // none and keeps none: it opens each, and its revision covers them all.
        let earlier = self.earlier();
        // The kept sighting, taken out while the page reads, and put back where it stood or the
        // page was refused: the order itself stays in `kept` for a page cut from it.
        let seen = kept
            .as_deref_mut()
            .and_then(|kept| kept.seen.take())
            .filter(|_| earlier.is_empty());
        let read = super::reopened(crate::admission::deadline(), || {
            // Asked after only where it is of this table at the home this read selects and the
            // order of its revision is kept. Any other page reads the store's records, so it
            // opens the store whatever a sighting says (a revision read kept this one and no
            // order: one open, never a sighting judged and then the open); and an earlier
            // attempt that read another revision let the order go before it was refused as
            // busy, so a sighting that stood then would name an order no longer held.
            let holds = seen.as_ref().is_some_and(|seen| {
                kept.as_deref()
                    .and_then(|kept| kept.order.as_ref())
                    .is_some_and(|order| order.of(&self.paths, compiled, seen.revision()))
            });
            let asked = |home: &Paths| {
                seen.as_ref()
                    .filter(|seen| holds && seen.is_of(home, compiled))
                    .map(|seen| &seen.sighting)
            };
            application::observe_store_sighted(
                &self.paths,
                self.descriptor(),
                asked,
                |session, read, scope| {
                    let store = match read {
                        // The store stands as the sighting's open read it: the kept order, of the
                        // sighting's revision, is the order now.
                        Sighted::Unchanged => return Ok(Served::Held(session.paths().clone())),
                        Sighted::Opened(store) => store,
                    };
                    let (revision, sighted) = store.sighted_revision(session)?;
                    let sealed = super::kept::sealed(session, &earlier)?;
                    let sighted =
                        Seen::of(session.paths(), compiled, sighted).filter(|_| sealed.is_empty());
                    let revision = Revision(super::kept::revision(revision, session, &sealed)?);
                    if let Some(kept) = kept.as_deref_mut() {
                        if let Some(order) = &kept.order
                            && order.of(&self.paths, compiled, revision)
                        {
                            return Ok(Served::Cut(
                                Cut::of(&order.entries, revision, ask)
                                    .copied(&order.entries)
                                    .refusing(order.refused.clone()),
                                sighted,
                            ));
                        }
                        // Another revision's order goes before this one is read: one is held at
                        // most.
                        kept.order = None;
                        #[cfg(test)]
                        if let Some(meanwhile) = ORDER_LET_GO.take() {
                            meanwhile()?;
                        }
                    }
                    let log = Log::read_with(session, &store, scope, compiled.decoder())?;
                    let resolve = owned::commons::historical(session);
                    // The names come from the same read, so an entry and its writer's name are
                    // of one revision.
                    let macs = Macs::of(&View::read(table, session, &log)?);
                    // The records the read holds and does not project, said with every page of
                    // it.
                    let mut refused =
                        log.refusals(&log.refused(&log.awaited(session.actor())), &resolve);
                    let entries = super::kept::merged(
                        ordered(compiled, &log, &resolve, &macs)?,
                        &mut refused,
                        session,
                        &sealed,
                        &resolve,
                        &macs,
                    )?;
                    let cut = Cut::of(&entries, revision, ask);
                    Ok(if kept.is_some() {
                        let page = cut.copied(&entries).refusing(refused.clone());
                        Served::Read(page, revision, entries, refused, sighted)
                    } else {
                        Served::Cut(cut.taken(entries).refusing(refused), None)
                    })
                },
            )
        });
        let served = match read {
            Ok(served) => served,
            Err(error) => {
                // A refused page leaves the sighting as it was: it is of an open that stood,
                // whatever this one met, and the next read asks it or opens the store.
                if let Some(kept) = kept {
                    kept.seen = seen;
                }
                return Err(error);
            }
        };
        Ok(match served {
            Served::Held(home) => {
                let unkept = || Error::new("a history sighting stood with no order kept");
                let kept = kept.ok_or_else(unkept)?;
                // The sighting that stood is the one asked: of this table at the home the read
                // selected. The page is cut from the order of that sighting's revision and no
                // other.
                let revision = seen
                    .as_ref()
                    .filter(|seen| seen.is_of(&home, compiled))
                    .map(Seen::revision);
                kept.seen = seen;
                let order = kept
                    .order
                    .as_ref()
                    .filter(|order| {
                        revision.is_some_and(|revision| order.of(&self.paths, compiled, revision))
                    })
                    .ok_or_else(unkept)?;
                Cut::of(&order.entries, order.revision, ask)
                    .copied(&order.entries)
                    .refusing(order.refused.clone())
            }
            // The store was opened: the sighting of that open is the one kept, or none where it
            // made none.
            Served::Cut(page, sighted) => {
                if let Some(kept) = kept {
                    kept.seen = sighted;
                }
                page
            }
            // Kept only once the observation stands: one refused as busy may have read a table
            // that moved under it.
            Served::Read(page, revision, entries, refused, sighted) => {
                if let Some(kept) = kept {
                    kept.order = Some(Order {
                        paths: self.paths.clone(),
                        table: compiled,
                        revision,
                        entries,
                        refused,
                    });
                    kept.seen = sighted;
                    kept.reads += 1;
                }
                page
            }
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

    /// The current view, its storage usage and its eligible edits in causal order, from one
    /// store observation; in a family that keeps its records across a roll, with the edits of
    /// its sealed generations as [`Self::history`] lists them ([`super::kept`]).
    /// Writer names in the history use the machine bindings in the returned view.
    ///
    /// # Errors
    /// As [`Self::view`] and [`Self::history`].
    pub fn view_with_history(&self) -> Result<HistoryView> {
        self.view_with_history_and(|_, _| ()).map(|(read, ())| read)
    }

    /// [`Self::view_with_history`], and with it what `of` reads of the same log under the
    /// session that read it (the checkpoint fence in [`Handle::view_with_history_fenced`]).
    ///
    /// # Errors
    /// As [`Self::view_with_history`].
    pub(super) fn view_with_history_and<T>(
        &self,
        of: impl Fn(&crate::table::Session<'_>, &Log) -> T,
    ) -> Result<(HistoryView, T)> {
        let compiled = self.compiled;
        let earlier = self.earlier();
        super::reopened(crate::admission::deadline(), || {
            application::observe_store(&self.paths, self.descriptor(), |session, store, scope| {
                // History requires the custody and file checks of the store's revision.
                store.revision(session)?;
                let sealed = super::kept::sealed(session, &earlier)?;
                let log = Log::read_with(session, store, scope, compiled.decoder())?;
                // The head a reader is shown: its own log over the owned rows and log entries
                // of the sealed generations ([`super::kept::Rows`]).
                let head = View::read_over(
                    compiled.table,
                    session,
                    &log,
                    super::kept::rows(session, &sealed)?,
                )?;
                let resolve = owned::commons::historical(session);
                let machines = head.project_settled(&MACHINE);
                let macs = Macs::from_projected(&head, &machines.0);
                let entries = super::kept::merged(
                    ordered(compiled, &log, &resolve, &macs)?,
                    &mut Vec::new(),
                    session,
                    &sealed,
                    &resolve,
                    &macs,
                )?;
                let read = of(session, &log);
                Ok((
                    HistoryView {
                        view: head,
                        usage: Usage::of(session.actor(), &log),
                        entries,
                        macs,
                        machines,
                    },
                    read,
                ))
            })
        })
    }

    /// The revision this Mac holds the table at, read without opening a payload: a reader that
    /// read a page or a view at this revision has nothing new to read. Each call opens the
    /// store; a reader that asks again and again keeps a sighting between its asks
    /// ([`Self::revision_kept`]). In a family that keeps its records across a roll it covers
    /// every generation's store ([`super::kept::revision`]).
    ///
    /// # Errors
    /// As [`Self::view`]; for a family that keeps its records, also where this Mac does not hold
    /// a sealed generation yet.
    pub fn revision(&self) -> Result<Revision> {
        let earlier = self.earlier();
        super::reopened(crate::admission::deadline(), || {
            application::observe_store(&self.paths, self.descriptor(), |session, store, _| {
                let sealed = super::kept::sealed(session, &earlier)?;
                super::kept::revision(store.revision(session)?, session, &sealed).map(Revision)
            })
        })
    }

    /// [`Self::revision`] for a reader that asks it again and again and keeps `kept` between its
    /// asks (tracker LT-25D: the Tables page's change check). Where `kept` holds a sighting of
    /// this table's store at the home this read selects (this handle's, or its successor's once
    /// its writer was succeeded) and the store stands as that sighting's open read it
    /// ([`crate::table::store::Store::sighting_stands`]), the revision is the sighting's and no
    /// store is opened, no record authenticated. Anywhere else (nothing kept, a sighting of
    /// another home or table, which is never asked, or a store that differs from it in anything
    /// or refuses) the store is opened as [`Self::revision`] opens it: that open's answer, a
    /// refusal included, is the answer, and `kept` takes the sighting of that open, or none
    /// where it made none (a combined view, a store holding a conflict certificate). The
    /// sighting is the one History's pages ask and keep ([`Self::history_page_kept`]), so a page
    /// and a revision read are each answered from the other's open while the store stands.
    ///
    /// It reads no record's payload and builds no order. Where the store is opened at a
    /// revision the kept order is not of, that order is let go: no page is cut from it again.
    ///
    /// At 105,043 records an ask that stands on the kept sighting took 133 ms where
    /// [`Self::revision`] took 1.84 s, in the `opt-test` profile
    /// (`att::tests::history_pages_at_100000_records_cost`): on that test's own thread, in a
    /// process that had opened the store before. The Tables page asks on a thread at background
    /// priority, where neither was measured.
    ///
    /// # Errors
    /// As [`Self::revision`]. On an error `kept` holds what it held.
    pub fn revision_kept(&self, kept: &mut Kept) -> Result<Revision> {
        let compiled = self.compiled;
        if !self.earlier().is_empty() {
            // A sighting is of one store; a family read across a roll is of several
            // ([`super::kept`]), so each is opened and no sighting is kept.
            let revision = self.revision()?;
            if kept
                .order
                .as_ref()
                .is_some_and(|order| !order.of(&self.paths, compiled, revision))
            {
                kept.order = None;
            }
            kept.seen = None;
            return Ok(revision);
        }
        // The kept sighting, asked only where it is of this table at the home this read selects.
        let seen = kept.seen.as_ref();
        let asked = |home: &Paths| seen.filter(|seen| seen.is_of(home, compiled));
        let (home, read) = super::reopened(crate::admission::deadline(), || {
            application::observe_store_sighted(
                &self.paths,
                self.descriptor(),
                |home| asked(home).map(|seen| &seen.sighting),
                |session, read, _| {
                    let read = match read {
                        Sighted::Unchanged => None,
                        Sighted::Opened(store) => Some(store.sighted_revision(session)?),
                    };
                    Ok((session.paths().clone(), read))
                },
            )
        })?;
        let Some((revision, sighted)) = read else {
            // The store stands as the asked sighting's open read it.
            return asked(&home)
                .map(Seen::revision)
                .ok_or_else(|| Error::new("a table sighting stood with none asked"));
        };
        let revision = Revision(revision);
        if kept
            .order
            .as_ref()
            .is_some_and(|order| !order.of(&self.paths, compiled, revision))
        {
            kept.order = None;
        }
        kept.seen = Seen::of(&home, compiled, sighted);
        Ok(revision)
    }
}

/// What one page's observation served ([`Handle::history_page_kept`]).
enum Served {
    /// The store stands as the kept sighting read it, and the kept order is of that sighting's
    /// revision: the page is cut from that order. With the home the read selected, whose store
    /// stood.
    Held(Paths),
    /// The page, cut from the kept order where the store was opened at its revision, or taken
    /// from an order read with nothing kept; with the sighting of the store it opened.
    Cut(Page, Option<Seen>),
    /// The page, and the order of the revision the store was read at, the records that read
    /// held and does not project, and its sighting: what `kept` keeps once the observation
    /// stands.
    Read(
        Page,
        Revision,
        Vec<Entry>,
        Vec<crate::typed::Unread>,
        Option<Seen>,
    ),
}

#[cfg(test)]
thread_local! {
    /// Run once by this thread's next page attempt that lets the kept order go, right after
    /// ([`with_order_let_go`]); its error is the attempt's.
    static ORDER_LET_GO: std::cell::Cell<Option<Box<dyn FnOnce() -> Result<()>>>> =
        const { std::cell::Cell::new(None) };
}

/// `read`, with `meanwhile` run by this thread's next page attempt that lets the kept order go,
/// right after it does, its error that attempt's: what happens between a page's attempts.
#[cfg(test)]
pub(super) fn with_order_let_go<T>(
    meanwhile: impl FnOnce() -> Result<()> + 'static,
    read: impl FnOnce() -> T,
) -> T {
    ORDER_LET_GO.set(Some(Box::new(meanwhile)));
    let read = read();
    assert!(
        ORDER_LET_GO.take().is_none(),
        "no page attempt let the kept order go"
    );
    read
}

/// Every eligible entry of `log`, the log of the generation `compiled`, in history's order
/// (oldest first); `resolve` finds a record's writer machine through the roster it names, and
/// `macs` that machine's Mac.
pub(super) fn ordered(
    compiled: &'static super::Compiled,
    log: &Log,
    resolve: &dyn Fn(&Digest, &Actor) -> Option<[u8; 16]>,
    macs: &Macs,
) -> Result<Vec<Entry>> {
    let table: &'static Table = compiled.table;
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
    let mut entries = Vec::with_capacity(order.len());
    for (hlc, id) in order {
        let Some((operation, command)) = log.eligible_record(&id) else {
            continue;
        };
        let Some(changes) = command.changes() else {
            continue;
        };
        let actor = operation.header.actor;
        let machine = resolve(&operation.roster, &actor);
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
            generation: compiled.generation,
            actor,
            machine,
            mac: macs.name(&actor, machine.as_ref()),
            authored_ms: operation.authored_ms,
            hlc,
            request: operation.request,
            reverts: operation.reverts,
            kind: EntryKind::of(table, is_revert(command), changes),
            inert,
            changes: changes.to_vec(),
            said: command.said().cloned(),
        });
    }
    Ok(entries)
}

impl Page {
    /// This page, saying the records the read it was cut from holds and does not project.
    fn refusing(mut self, refused: Vec<crate::typed::Unread>) -> Self {
        self.refused = refused;
        self
    }
}

/// Which entries the page an [`Ask`] names holds, as places in the order it was cut from.
pub(super) struct Cut {
    revision: Revision,
    moved: bool,
    newest_first: bool,
    /// The page's entries' places, in the page's order: rising oldest first, falling newest
    /// first.
    at: Vec<usize>,
    next: Option<Cursor>,
}

impl Cut {
    /// The page `ask` names of `entries`, every eligible entry of the table at `revision` in
    /// history's order.
    pub(super) fn of(entries: &[Entry], revision: Revision, ask: &Ask) -> Self {
        let moved = ask.revision.is_some_and(|asked| asked != revision);
        let after = ask.after.filter(|_| !moved);
        // Oldest first, the entries after the cursor; newest first, those before it, run
        // backwards.
        let served: Box<dyn Iterator<Item = usize>> = if ask.newest_first {
            let end = after.map_or(entries.len(), |after| {
                entries.partition_point(|entry| entry.cursor() < after)
            });
            Box::new((0..end).rev())
        } else {
            let start = after.map_or(0, |after| {
                entries.partition_point(|entry| entry.cursor() <= after)
            });
            Box::new(start..entries.len())
        };
        let mut at: Vec<usize> = Vec::new();
        let mut next = None;
        for n in served.filter(|&n| ask.filter.keeps(&entries[n])) {
            if at.len() == ask.limit {
                next = at.last().map(|&last| entries[last].cursor());
                break;
            }
            at.push(n);
        }
        Self {
            revision,
            moved,
            newest_first: ask.newest_first,
            at,
            next,
        }
    }

    /// The page, its entries copied from `entries`, the order it was cut from.
    pub(super) fn copied(self, entries: &[Entry]) -> Page {
        let page = self.at.iter().map(|&n| entries[n].clone()).collect();
        self.page(page)
    }

    /// The page, its entries taken out of `entries`, the order it was cut from, which goes.
    pub(super) fn taken(self, entries: Vec<Entry>) -> Page {
        let mut page = Vec::with_capacity(self.at.len());
        {
            // The order is walked once, oldest first; a page asked newest first is turned back
            // after.
            let mut wanted: Box<dyn Iterator<Item = &usize>> = if self.newest_first {
                Box::new(self.at.iter().rev())
            } else {
                Box::new(self.at.iter())
            };
            let mut want = wanted.next();
            for (n, entry) in entries.into_iter().enumerate() {
                let Some(&at) = want else {
                    break;
                };
                if n == at {
                    page.push(entry);
                    want = wanted.next();
                }
            }
        }
        if self.newest_first {
            page.reverse();
        }
        self.page(page)
    }

    /// The page of `entries`, its entries in its order.
    fn page(self, entries: Vec<Entry>) -> Page {
        Page {
            revision: self.revision,
            moved: self.moved,
            entries,
            next: self.next,
            refused: Vec::new(),
        }
    }
}

const fn is_revert(command: &Command) -> bool {
    matches!(command, Command::Revert { .. })
}
