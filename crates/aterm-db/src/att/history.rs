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
//! order the table has moved past.

use super::{Handle, View, decode};
use crate::{
    Error, Result,
    alab::registry::{self, MACHINE, MACHINE_FACTS},
    database::{Change, Command, decode_hex},
    documents::{application, log::Log},
    typed::{
        self, Table,
        cell::{CellKey, CellValue, Projection},
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
    /// The `machine` row the writer's `machine-facts` row binds ([`registry::bound_machine`]:
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
        let empty = OwnedRead::default();
        Self::from_rows(
            &view.project(&MACHINE),
            view.owned.get(MACHINE_FACTS.name).unwrap_or(&empty),
        )
    }

    /// The join `facts` (each Mac's own statement, keyed by its Commons machine) and `machines`
    /// (the registry's machine rows) hold. A facts row's `predecessor` is the same Mac before
    /// writer succession: a predecessor whose own facts row binds no machine row, or that has
    /// none, binds what its successor binds, however far back the chain runs.
    pub fn from_rows(machines: &Projection, facts: &OwnedRead) -> Self {
        let mut bound: BTreeMap<[u8; 16], String> = facts
            .rows
            .values()
            .filter_map(|row| {
                let key = registry::bound_machine(&row.value).filter(|key| !key.is_empty())?;
                Some((row.machine, key.to_owned()))
            })
            .collect();
        // Each pass carries a binding one predecessor back; a chain is no longer than the rows.
        for _ in 0..facts.rows.len() {
            let mut carried = false;
            for row in facts.rows.values() {
                let Some(before) = row
                    .value
                    .get("predecessor")
                    .and_then(aterm_json::Value::as_str)
                    .and_then(keys::parse_uuid32)
                else {
                    continue;
                };
                if bound.contains_key(&before) {
                    continue;
                }
                if let Some(key) = bound.get(&row.machine).cloned() {
                    bound.insert(before, key);
                    carried = true;
                }
            }
            if !carried {
                break;
            }
        }
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
}

/// The whole of one revision of one table's history, ordered and resolved: what every page at
/// that revision is cut from ([`Handle::history_page_kept`]). It holds one revision of one
/// table at a time; a page at any other revision, or of another table or home, replaces it.
/// Empty until a page fills it, and its order goes when it is dropped.
///
/// That one revision is every eligible entry of the table's history, those below a final
/// checkpoint's floor included (the store's quotas bound only the records above it), so its
/// memory grows with the table's history: 50.8 MiB at 105,043 entries of one small change each,
/// more where an entry's changes and its writer's Mac are wider. Its reader holds it for as
/// long as it reads.
#[derive(Default)]
pub struct Kept {
    order: Option<Order>,
    reads: u64,
}

impl Kept {
    /// The revision the kept order is of, where one is kept.
    pub fn revision(&self) -> Option<Revision> {
        self.order.as_ref().map(|order| order.revision)
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
}

impl Order {
    /// Whether this is the order of `table` at `paths` at `revision`. The revision digests the
    /// table's scope and this Mac's access with every record, so it alone names the order; the
    /// home and the table are compared too, since a [`Kept`] may be handed any handle.
    ///
    /// The table is compared by what names its store (application, role and descriptor version)
    /// and its generation, never by address: a compiled table is a `const` (`att::REGISTRY`),
    /// and each `&REGISTRY` may be a copy of its own, so a handle made at one place and a
    /// handle made at another are of the same table at different addresses.
    fn of(&self, paths: &Paths, table: &super::Compiled, revision: Revision) -> bool {
        let kept = self.table;
        self.revision == revision
            && kept.table.application == table.table.application
            && kept.table.role == table.table.role
            && kept.version == table.version
            && kept.generation == table.generation
            && self.paths == *paths
    }
}

impl Handle {
    /// One page of this table's eligible history ([`Ask`]), in derived causal-clock order with
    /// concurrent edits ordered by clock then digest (newest first where [`Ask::newest_first`]),
    /// read in one store read. A reader that asks more than one page keeps their order between
    /// them ([`Self::history_page_kept`]).
    ///
    /// # Errors
    /// As [`Self::view`]; also refuses incomplete causal clocks, a logical counter overflow, or
    /// a page of no entries.
    pub fn history_page(&self, ask: &Ask) -> Result<Page> {
        self.read_history_page(ask, None)
    }

    /// [`Self::history_page`] cut from `kept` where it holds this table's order at the revision
    /// the store is at, which is read without opening a payload: such a page decodes, projects
    /// and orders no record. Else the store is read in the same observation, and `kept` takes
    /// the order of that revision once the observation stands, its earlier order let go first,
    /// so it never holds two. A page is never cut from an order of any revision but the one its
    /// own observation read.
    ///
    /// # Errors
    /// As [`Self::history_page`]. On an error `kept` holds no order or the one it held, never a
    /// part of one.
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
        let read = super::reopened(crate::admission::deadline(), || {
            application::observe_store(&self.paths, self.descriptor(), |session, store, scope| {
                let revision = Revision(store.revision(session)?);
                if let Some(kept) = kept.as_deref_mut() {
                    if let Some(order) = &kept.order
                        && order.of(&self.paths, compiled, revision)
                    {
                        return Ok((
                            Cut::of(&order.entries, revision, ask).copied(&order.entries),
                            None,
                        ));
                    }
                    // Another revision's order goes before this one is read: one is held at
                    // most.
                    kept.order = None;
                }
                let log = Log::read_with(session, store, scope, decode)?;
                let resolve = owned::commons::historical(session);
                // The names come from the same read, so an entry and its writer's name are of
                // one revision.
                let macs = Macs::of(&View::read(table, session, &log)?);
                let entries = ordered(table, &log, &resolve, &macs)?;
                let cut = Cut::of(&entries, revision, ask);
                Ok(if kept.is_some() {
                    (cut.copied(&entries), Some((revision, entries)))
                } else {
                    (cut.taken(entries), None)
                })
            })
        });
        let (page, read) = read?;
        // Kept only once the observation stands: one refused as busy may have read a table that
        // moved under it.
        if let (Some(kept), Some((revision, entries))) = (kept, read) {
            kept.order = Some(Order {
                paths: self.paths.clone(),
                table: compiled,
                revision,
                entries,
            });
            kept.reads += 1;
        }
        Ok(page)
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

/// Every eligible entry of `log`, in history's order (oldest first); `resolve` finds a record's
/// writer machine through the roster it names, and `macs` that machine's Mac.
fn ordered(
    table: &'static Table,
    log: &Log,
    resolve: &dyn Fn(&Digest, &Actor) -> Option<[u8; 16]>,
    macs: &Macs,
) -> Result<Vec<Entry>> {
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
        });
    }
    Ok(entries)
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
        }
    }
}

const fn is_revert(command: &Command) -> bool {
    matches!(command, Command::Revert { .. })
}
