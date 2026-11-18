// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The history drawer's fourth scope, Registry (tracker package LT-25, design §3.3): every record
//! of the registry, newest first in the causal order of LT-10, read in pages (LT-26).
//!
//! - **Pages.** The drawer never reads the whole registry: its first page is the newest
//!   [`PAGE`] records the filters keep (`aterm_db::att::Ask::newest`), and Read older records at
//!   the list's foot asks the page before the oldest one read, at the revision the earlier
//!   pages were read at. A read of the page that finds the registry at another revision than
//!   History was read at (another machine's records, a Write) asks again from the top through
//!   the oldest record listed (`Asked::through`), however far back Read older records went, so
//!   every line that showed stays: new records come in above, the selection and the ringed line
//!   stay ([`super::Drawer::follow`] keeps what showed where it stood). An older page the store
//!   answers at another revision ([`Paged::moved`]) is not put under the list: the list is read
//!   again that way first, then the older page. Only while History shows does a move read it
//!   again; a History hidden reads again once it shows. A press of Read older records while a
//!   read is out waits for it, and is not lost.
//! - **A line is one record:** when, the writing machine, its kind (no word for an explicit
//!   write, `aterm` or `import`, as [`Author::who`] writes them on every other line), the tables,
//!   the row or the count of rows, what changed in words, and the count of cells. An owned row's
//!   write is one whole-row change, and it is aterm's (`aterm_db::att::EntryKind`). One press of
//!   Write is one save per table, each save one or more records of at most 64 changes, so a
//!   press can be several lines: nothing here joins them, and nothing calls a line a press.
//! - **Folds** are display only: [`RUN`] or more consecutive records of one machine and kind
//!   under one day fold into a line that says it is a fold of so many records, never one
//!   write; it opens on a press. A fold is open while any record in it was the one a person
//!   opened it by, so records joining it above (a re-read) or below (Read older records) leave
//!   it open.
//! - **Days.** Lines are grouped by the day of their claimed time, within the causal order: the
//!   list is never sorted by a claim. A claim more than a day from both its causal neighbours'
//!   that also runs against the causal order by more than a day (later than the record caused
//!   after it, or earlier than the one it came after) is shown as claimed, its whole date on its
//!   line, and starts no day of its own: a machine's clock that ran wrong does not cut the days.
//!   Records written days apart in order, a quiet registry's, are not: each starts its day.
//! - **Filters:** table and kind are the store's own filters (`aterm_db::att::Filter`), applied
//!   before a page is cut; machine (a machine row, which several Commons machines can bind) and
//!   Find (the words a line says, lowered) are this reader's, applied to the store's pages until
//!   a page of matches is found or [`SCAN`] records were looked at, so a filter that matches
//!   little still answers within a bounded read, and Read older records goes on from there, also
//!   where that read found none. A filter changed (each key typed in Find) leaves the lines read
//!   until its read answers, and the reader reads only the last of the asks queued behind one
//!   another.
//! - **A selected record** shows, under its line, each cell it wrote as was and now (what it
//!   replaced, from `aterm_db::att::Handle::wrote_named`), the writes to the same cells it had
//!   not seen and what has written to them since; each such line leads to its row where the
//!   rail lists its table, and says that it does not where it does not. A record that projects
//!   nothing says `not counted`. Selecting a record opens, for display, a fold that later forms
//!   around it.

use std::collections::BTreeMap;

use super::super::{Author, AuthorKind, TablesApp, TablesViewState, Work};
use super::{Line, Scope};

/// How many records one read of History lists: the page `aterm db table history` prints.
pub(crate) const PAGE: usize = 100;
/// How many records one read looks at for the filters this reader applies (machine, Find)
/// before it answers with what it found: a bounded read, which Read older records goes on from.
pub(crate) const SCAN: usize = 2048;
/// A run of this many consecutive records of one machine and kind folds: two lines take no
/// more room than a fold's line and one of its records, so a fold of two would hide a record
/// for nothing.
pub(crate) const RUN: usize = 3;
/// A claim further than this from both causal neighbours' claims, and against their order by
/// as much, is shown as claimed.
const DAY_MS: u64 = 86_400_000;
/// The table of machines, whose rows the machine filter offers: the rows a writer's facts bind.
#[cfg(unix)]
const MACHINES: &str = aterm_db::alab::registry::MACHINE.name;
#[cfg(not(unix))]
const MACHINES: &str = "machine";

/// A place in the registry's order, as the store's cursor names it: the causal clock's time and
/// count, and the operation's digest.
pub(crate) type Cursor = (u64, u64, [u8; 32]);

/// One cell a record wrote, as History says it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LoggedCell {
    pub(crate) collection: String,
    /// The row: a cells collection's row key, an owned or log row's key.
    pub(crate) pk: String,
    /// The typed cell's column; `None` for a whole row.
    pub(crate) column: Option<String>,
    /// What changed, in words: `Location → rack b`, `Hosts → + h1`, `Location → empty`,
    /// `whole row`.
    pub(crate) words: String,
}

/// One record of the registry, as History lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Logged {
    pub(crate) operation: [u8; 32],
    /// Its writing machine, its kind, its claim and its place in the causal order.
    pub(crate) by: Author,
    /// The machine row its writer's facts bind, where one does: what the machine filter keeps.
    pub(crate) machine: Option<String>,
    /// It projects nothing (`aterm_db::att::Inert`): not counted.
    pub(crate) inert: bool,
    /// Each cell it wrote, in its record's order.
    pub(crate) cells: Vec<LoggedCell>,
}

/// One page of History, as the reader read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Paged {
    /// Newest first.
    pub(crate) records: Vec<Logged>,
    /// Where the page before it would start; `None` once nothing older is kept.
    pub(crate) next: Option<Cursor>,
    /// The registry's revision the page was read at.
    pub(crate) revision: [u8; 32],
    /// It was asked at another revision, or the registry moved while it was read: it starts from
    /// the newest record and takes the place of every page read before.
    pub(crate) moved: bool,
    /// Every table History can list records of, by name and words, the rail's first in its
    /// order, then those the rail does not list (a log, a carried face): what the table filter
    /// offers.
    pub(crate) tables: Vec<(String, String)>,
}

/// What History keeps: the drawer's filters, as the reader applies them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Filter {
    /// One table (a collection's name).
    pub(crate) table: Option<String>,
    /// One machine row.
    pub(crate) machine: Option<String>,
    pub(crate) kind: Option<AuthorKind>,
    /// Find's words, trimmed and lowered: a record is kept where its line's words hold them.
    pub(crate) find: String,
}

impl Filter {
    /// Whether this reader, not the store, decides what is kept: a machine or Find.
    pub(crate) const fn reads(&self) -> bool {
        self.machine.is_some() || !self.find.is_empty()
    }

    /// Whether `record`, whose line says `words` ([`words_of`]), passes the machine and Find.
    pub(crate) fn keeps(&self, record: &Logged, words: &str) -> bool {
        self.machine
            .as_ref()
            .is_none_or(|machine| record.machine.as_ref() == Some(machine))
            && (self.find.is_empty() || words.to_lowercase().contains(&self.find))
    }
}

/// One read of History asked of the page's reader.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Asked {
    /// Which ask it answers: a reply to an older one is dropped.
    pub(crate) seq: u64,
    pub(crate) filter: Filter,
    /// The words the page reads each row by, by collection and key, as the drawer names a row:
    /// sent only where Find needs them, so Find matches what a line says.
    pub(crate) names: BTreeMap<(String, String), String>,
    /// The records before this one; from the newest where `None`.
    pub(crate) before: Option<Cursor>,
    /// The revision the pages read so far were read at.
    pub(crate) revision: Option<[u8; 32]>,
    /// At least `limit` records, and every one from the newest through this record (the oldest
    /// listed), however many that is or how many the filters look at: a read again from the top
    /// keeps every line that showed.
    pub(crate) through: Option<Cursor>,
    pub(crate) limit: usize,
}

/// One write beside a selected record's: its machine's name and the value it wrote in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Valued {
    pub(crate) by: String,
    pub(crate) words: String,
}

/// One cell a selected record wrote, as History shows it opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OpenedCell {
    pub(crate) collection: String,
    pub(crate) pk: String,
    pub(crate) column: Option<String>,
    /// The column's words, or `Whole row`.
    pub(crate) what: String,
    /// What it wrote there.
    pub(crate) now: String,
    /// What that replaced.
    pub(crate) was: Vec<Valued>,
    /// The writes to the cell it had not seen, which nor saw it.
    pub(crate) unseen: Vec<Valued>,
    /// The writes to the cell that saw it: what has written there since.
    pub(crate) since: Vec<Valued>,
}

/// A selected record, opened: each cell it wrote ([`OpenedCell`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Opened {
    pub(crate) cells: Vec<OpenedCell>,
}

/// Which of the drawer's filters lists its choices in the list's place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Which {
    Table,
    Machine,
}

impl Which {
    pub(crate) const fn slug(self) -> &'static str {
        match self {
            Self::Table => "table",
            Self::Machine => "machine",
        }
    }
}

/// Each kind the kind filter offers, with its word on the control and its slug.
pub(crate) const KINDS: [(Option<AuthorKind>, &str, &str); 4] = [
    (None, "Any kind", "any"),
    (Some(AuthorKind::Explicit), "Explicit", "explicit"),
    (Some(AuthorKind::Aterm), "aterm", "aterm"),
    (Some(AuthorKind::Import), "import", "import"),
];

/// What a page the reader answered did to the list ([`Registry::take_page`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Took {
    /// It answers an ask no longer in flight: dropped.
    Dropped,
    /// Taken: listed, or why it failed kept.
    Taken,
    /// An older page found the registry moved: nothing taken, the list is read again first.
    Moved,
}

/// Which read a reply answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Asking {
    /// The newest page: what is listed goes.
    Top,
    /// The page before the oldest listed: it comes in under them.
    Older,
}

/// What History has read of the registry: the page's, kept as its reader answers.
pub(crate) struct Registry {
    /// The filters the records listed were read with.
    pub(crate) filter: Filter,
    /// The records read, newest first.
    pub(crate) records: Vec<Logged>,
    next: Option<Cursor>,
    revision: Option<[u8; 32]>,
    /// Whether History was asked for once: a read that finds the registry moved reads it again.
    pub(crate) read: bool,
    /// Whether a drawer shows History: a move reads it again only then ([`TablesApp::registry_shown`]).
    watching: bool,
    /// The registry moved while no drawer showed History: it reads again once one does.
    stale: bool,
    /// Read older records was pressed while a read was out, or an older page found the registry
    /// moved: the older page is asked once that read answers.
    then_older: bool,
    /// Every table History can list, from its last page ([`Paged::tables`]).
    tables: Vec<(String, String)>,
    /// The read in flight, by its number, and what it asked.
    asking: Option<(u64, Asking)>,
    seq: u64,
    /// How many pages were taken: the drawer keeps what showed in place across one.
    pub(crate) reads: u64,
    /// Why the last read failed.
    pub(crate) why: Option<String>,
    /// The record selected, and what its read found of it once it answers.
    pub(crate) selected: Option<[u8; 32]>,
    pub(crate) opened: Option<Result<Opened, String>>,
    /// How many reads of a record are in flight.
    opening: usize,
    /// How many records a read lists.
    pub(crate) page: usize,
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            filter: Filter::default(),
            records: Vec::new(),
            next: None,
            revision: None,
            read: false,
            watching: false,
            stale: false,
            then_older: false,
            tables: Vec::new(),
            asking: None,
            seq: 0,
            reads: 0,
            why: None,
            selected: None,
            opened: None,
            opening: 0,
            page: PAGE,
        }
    }
}

impl Registry {
    /// Whether a read is in flight.
    pub(crate) const fn reading(&self) -> bool {
        self.asking.is_some()
    }

    /// Whether older records are left to read.
    pub(crate) const fn more(&self) -> bool {
        self.next.is_some()
    }

    /// Take a page the reader answered: an answer to an older ask is dropped, and an older page
    /// that found the registry moved is not put under the list ([`Took::Moved`]).
    pub(crate) fn take_page(&mut self, seq: u64, read: Result<Paged, String>) -> Took {
        let Some((asked, asking)) = self.asking else {
            return Took::Dropped;
        };
        if asked != seq {
            return Took::Dropped;
        }
        self.asking = None;
        match read {
            Ok(page) if asking == Asking::Older && page.moved => {
                // Its records are the newest, not those under the list: the list is read again
                // through its oldest record, then the page under it.
                self.then_older = true;
                return Took::Moved;
            }
            Ok(page) => {
                self.why = None;
                if asking == Asking::Top {
                    self.records = page.records;
                } else {
                    // An older page holds only records before the oldest listed, in the causal
                    // order the store keeps by clock and digest: none is listed twice.
                    let oldest = self.records.last().map(cursor_of);
                    self.records.extend(
                        page.records.into_iter().filter(|record| {
                            oldest.is_none_or(|oldest| cursor_of(record) < oldest)
                        }),
                    );
                }
                self.next = page.next;
                self.revision = Some(page.revision);
                self.tables = page.tables;
            }
            Err(why) => self.why = Some(why),
        }
        self.reads = self.reads.wrapping_add(1);
        Took::Taken
    }

    /// Take what the reader read of record `operation`, where it is still the one selected.
    pub(crate) fn take_record(
        &mut self,
        operation: [u8; 32],
        read: Result<Opened, String>,
    ) -> bool {
        self.opening = self.opening.saturating_sub(1);
        if self.selected != Some(operation) {
            return false;
        }
        self.opened = Some(read);
        true
    }
}

/// Where `record` stands in the registry's causal order: its clock, then its digest.
const fn cursor_of(record: &Logged) -> Cursor {
    (record.by.hlc.0, record.by.hlc.1, record.operation)
}

/// The words every row of the last read is named by in the drawer, by collection and key
/// ([`super::names`]), for Find to match what a line says.
fn row_names(app: &TablesApp) -> BTreeMap<(String, String), String> {
    let mut out = BTreeMap::new();
    for sheet in app.snapshot.iter().flat_map(|snapshot| &snapshot.sheets) {
        for (pk, words) in super::names(sheet) {
            out.insert((sheet.name.to_owned(), pk.to_owned()), words.to_owned());
        }
    }
    out
}

impl TablesApp {
    /// Ask the reader for History's newest page under the drawer's filters, through the oldest
    /// record listed, so a read again keeps every line that showed. A read of an older page out
    /// is asked again once this one answers: its press is not lost.
    pub(crate) fn registry_top(&mut self) {
        let through = self.registry.records.last().map(cursor_of);
        if self
            .registry
            .asking
            .is_some_and(|(_, asking)| asking == Asking::Older)
        {
            self.registry.then_older = true;
        }
        // As many as are listed, so the store's first page most often reaches that record.
        let limit = self.registry.page.max(self.registry.records.len());
        self.registry_ask(Asking::Top, None, None, through, limit);
    }

    /// Ask for the page before the oldest record listed, at the revision the list was read at;
    /// while a read is out, once it answers.
    pub(crate) fn registry_older(&mut self) {
        if self.registry.next.is_none() {
            return;
        }
        if self.registry.reading() {
            self.registry.then_older = true;
            return;
        }
        let (before, revision, page) = (
            self.registry.next,
            self.registry.revision,
            self.registry.page,
        );
        self.registry_ask(Asking::Older, before, revision, None, page);
    }

    /// Take a page the reader answered ([`Registry::take_page`]); an older page that found the
    /// registry moved reads the list again, and a press of Read older records that waited on a
    /// read is asked now.
    pub(crate) fn registry_take(&mut self, seq: u64, read: Result<Paged, String>) {
        match self.registry.take_page(seq, read) {
            Took::Dropped => {}
            Took::Moved => self.registry_top(),
            Took::Taken => {
                if std::mem::take(&mut self.registry.then_older) && self.registry.why.is_none() {
                    self.registry_older();
                }
            }
        }
    }

    fn registry_ask(
        &mut self,
        asking: Asking,
        before: Option<Cursor>,
        revision: Option<[u8; 32]>,
        through: Option<Cursor>,
        limit: usize,
    ) {
        let registry = &mut self.registry;
        registry.seq = registry.seq.wrapping_add(1);
        registry.read = true;
        registry.stale = false;
        let filter = registry.filter.clone();
        let names = if filter.find.is_empty() {
            BTreeMap::new()
        } else {
            row_names(self)
        };
        let asked = Asked {
            seq: self.registry.seq,
            filter,
            names,
            before,
            revision,
            through,
            limit,
        };
        if self.work.send(Work::History(asked)).is_ok() {
            self.registry.asking = Some((self.registry.seq, asking));
        }
    }

    /// The page read the registry at `revision` (`None` where its mark could not be read): where
    /// History was read and is at another, it reads again from the top, and the selected record
    /// again, since what has written its cells since may have grown; while no drawer shows
    /// History, once one does. A read that moved nothing History lists (the page read as of a
    /// record, a masked number read again) reads nothing again.
    pub(crate) fn registry_moved(&mut self, revision: Option<[u8; 32]>) {
        if !self.registry.read || revision.is_some_and(|now| self.registry.revision == Some(now)) {
            return;
        }
        if !self.registry.watching {
            self.registry.stale = true;
            return;
        }
        self.registry_again();
    }

    fn registry_again(&mut self) {
        self.registry_top();
        // What was read of the selected record shows until this read answers.
        if let Some(operation) = self.registry.selected {
            self.registry_open(operation);
        }
    }

    /// Whether a drawer shows History now: shown, it reads the registry where it never did,
    /// where the registry moved while it was hidden, and where its last read failed.
    pub(crate) fn registry_shown(&mut self, on: bool) {
        self.registry.watching = on;
        if !on || self.registry.reading() {
            return;
        }
        let registry = &self.registry;
        if !registry.read || (registry.why.is_some() && registry.records.is_empty()) {
            self.registry_top();
        } else if registry.stale || registry.why.is_some() {
            self.registry_again();
        }
    }

    fn registry_open(&mut self, operation: [u8; 32]) {
        if self.work.send(Work::Record(operation)).is_ok() {
            self.registry.opening += 1;
        }
    }

    /// Take the reader's answers until History's reads in flight are answered: a test's hang
    /// detector, as [`Self::settle`] is.
    #[cfg(all(test, unix))]
    pub(crate) fn settle_registry(&mut self) {
        while self.registry.reading() || self.registry.opening > 0 {
            self.settle();
        }
    }

    /// Select record `operation`, or, where it is the one selected, put it down.
    pub(crate) fn registry_select(&mut self, operation: [u8; 32]) {
        if self.registry.selected == Some(operation) {
            self.registry.selected = None;
            self.registry.opened = None;
            return;
        }
        self.registry.selected = Some(operation);
        self.registry.opened = None;
        self.registry_open(operation);
    }

    /// Set the filters to `filter` and read from the top where they changed. The lines read
    /// stay until that read answers, so a key typed in Find does not empty the list; a press of
    /// Read older records waiting was of the filters before, and goes.
    pub(crate) fn registry_filter(&mut self, filter: Filter) {
        if self.registry.filter == filter && self.registry.read {
            return;
        }
        self.registry.filter = filter;
        self.registry.then_older = false;
        let page = self.registry.page;
        self.registry_ask(Asking::Top, None, None, None, page);
    }

    /// Each table History's table filter offers: every table its last page says it can list
    /// records of, the rail's first ([`Paged::tables`]); the rail's before it read one.
    pub(crate) fn registry_tables(&self) -> Vec<(String, String)> {
        if !self.registry.tables.is_empty() {
            return self.registry.tables.clone();
        }
        self.listed()
            .iter()
            .map(|listed| (listed.name.to_owned(), listed.title.to_owned()))
            .collect()
    }

    /// Each machine History's machine filter offers: the machine rows the last read holds, by
    /// key and the words the page names each by.
    pub(crate) fn registry_machines(&self) -> Vec<(String, String)> {
        self.sheet(MACHINES).map_or_else(Vec::new, |sheet| {
            let names = super::names(sheet);
            let mut rows: Vec<(String, String)> = sheet
                .rows
                .iter()
                .filter(|row| !super::rows::is_new(&row.pk))
                .map(|row| {
                    let words = names.get(row.pk.as_str()).copied().unwrap_or(&row.pk);
                    (row.pk.clone(), words.to_owned())
                })
                .collect();
            rows.sort_by(|a, b| super::super::natural_lowered(&a.1, &b.1));
            rows
        })
    }
}

/// What a line of History says of where `record` wrote and what it changed: the tables'
/// titles (`title` of each collection) and the row (`row` of a collection and key) or the
/// count of rows; then each cell's change in words.
pub(crate) fn place_words(
    record: &Logged,
    title: impl Fn(&str) -> String,
    row: impl Fn(&str, &str) -> String,
) -> (String, String) {
    let mut tables: Vec<&str> = Vec::new();
    let mut rows: Vec<(&str, &str)> = Vec::new();
    for cell in &record.cells {
        if !tables.contains(&cell.collection.as_str()) {
            tables.push(&cell.collection);
        }
        if !rows.contains(&(cell.collection.as_str(), cell.pk.as_str())) {
            rows.push((&cell.collection, &cell.pk));
        }
    }
    let tables = tables
        .iter()
        .map(|name| title(name))
        .collect::<Vec<_>>()
        .join(", ");
    let rows = match rows.as_slice() {
        [] => String::new(),
        [(collection, pk)] => row(collection, pk),
        many => format!("{} rows", many.len()),
    };
    let place = if rows.is_empty() {
        tables
    } else {
        format!("{tables} \u{b7} {rows}")
    };
    let changed = record
        .cells
        .iter()
        .map(|cell| cell.words.as_str())
        .collect::<Vec<_>>()
        .join("; ");
    (place, changed)
}

/// `1 cell`, `n cells`.
pub(crate) fn cells_words(n: usize) -> String {
    match n {
        1 => "1 cell".to_owned(),
        n => format!("{n} cells"),
    }
}

/// Everything a line of `record` says, which Find matches: its writer and kind
/// ([`Author::who`]), where it wrote, what it changed and its count of cells.
pub(crate) fn words_of(record: &Logged, place: &str, changed: &str) -> String {
    let mut words = format!(
        "{} {place} {changed} {}",
        record.by.who(),
        cells_words(record.cells.len())
    );
    if record.inert {
        words.push_str(" not counted");
    }
    words
}

/// Whether the claim at `at` of the records in list order `claims` (newest first) is a clock's
/// that ran wrong: more than a day from both its neighbours' (from its one neighbour's at either
/// end; never where it stands alone), and against the causal order by more than a day, later
/// than the record caused after it (above) or earlier than the one it came after (below).
/// Records a quiet registry wrote days apart stand in order, and are none.
fn apart(claims: &[u64], at: usize) -> bool {
    let here = claims[at];
    let newer = |k: usize| at.checked_sub(k).and_then(|n| claims.get(n)).copied();
    let older = |k: usize| claims.get(at + k).copied();
    let far = |other: Option<u64>| other.is_none_or(|other| here.abs_diff(other) > DAY_MS);
    let ahead = |newer: Option<u64>| newer.is_some_and(|newer| here > newer.saturating_add(DAY_MS));
    let behind =
        |older: Option<u64>| older.is_some_and(|older| here.saturating_add(DAY_MS) < older);
    match (newer(1), older(1)) {
        (None, None) => false,
        (Some(_), Some(_)) => {
            far(newer(1)) && far(older(1)) && (ahead(newer(1)) || behind(older(1)))
        }
        // At an end its one neighbour may be the clock that ran wrong: it is apart only where the
        // record past that neighbour, where there is one, finds it against the order too.
        (Some(_), None) => ahead(newer(1)) && (newer(2).is_none() || ahead(newer(2))),
        (None, Some(_)) => behind(older(1)) && (older(2).is_none() || behind(older(2))),
    }
}

/// The records of `records` (newest first) each line of the list stands for, top down: a run of
/// consecutive records of one machine and kind under one day (a fold where it is [`RUN`] or
/// more long), or one record; each with whether a day's head goes over it.
pub(crate) fn groups(records: &[Logged]) -> Vec<(std::ops::Range<usize>, Option<String>)> {
    let claims: Vec<u64> = records.iter().map(|r| r.by.at_ms).collect();
    let day_of = |n: usize| super::day_words(records[n].by.at_ms);
    let mut out = Vec::new();
    let mut day: Option<String> = None;
    let mut at = 0;
    while at < records.len() {
        let first = &records[at];
        // Where the day starts: the first record of a run whose claim is not apart.
        let mut head = None;
        if !apart(&claims, at) {
            let claimed = day_of(at);
            if day.as_ref() != Some(&claimed) {
                head = Some(claimed.clone());
                day = Some(claimed);
            }
        }
        // The run: records of one machine and kind, within the day.
        let mut end = at + 1;
        while end < records.len() {
            let next = &records[end];
            let same = next.by.kind == first.by.kind
                && (next.by.label.as_ref(), &next.by.id) == (first.by.label.as_ref(), &first.by.id);
            if !same || (!apart(&claims, end) && Some(day_of(end)) != day) {
                break;
            }
            end += 1;
        }
        out.push((at..end, head));
        at = end;
    }
    out
}

/// Whether the fold of `run` is open: a record in it is one it was opened by (`runs`).
pub(crate) fn open(run: &[Logged], runs: &BTreeMap<[u8; 32], bool>) -> bool {
    run.iter()
        .any(|record| runs.get(&record.operation).copied().unwrap_or(false))
}

/// History's list, top down: the records read, under the day each claims, consecutive records
/// of one machine and kind folded where [`RUN`] or more stand together (`runs` says which a
/// person opened, by their oldest record's digest), the selected record's cells under its line,
/// and at the foot the way to older records or the start of the registry. While a filter's
/// choices are open, those instead.
pub(crate) fn lines(
    app: &TablesApp,
    view: &TablesViewState,
    runs: &BTreeMap<[u8; 32], bool>,
) -> Vec<Line> {
    let registry = &app.registry;
    if let Some(which) = view.history.choosing {
        let chosen = match which {
            Which::Table => registry.filter.table.as_ref(),
            Which::Machine => registry.filter.machine.as_ref(),
        };
        let (all, offered) = match which {
            Which::Table => ("All tables", app.registry_tables()),
            Which::Machine => ("All machines", app.registry_machines()),
        };
        return std::iter::once((None, all.to_owned()))
            .chain(offered.into_iter().map(|(key, words)| (Some(key), words)))
            .enumerate()
            .map(|(n, (key, words))| Line::Choice {
                which,
                n,
                chosen: key.as_ref() == chosen,
                words,
            })
            .collect();
    }
    let mut out = Vec::new();
    if registry.records.is_empty() {
        out.push(Line::Note(match (&registry.why, registry.reading()) {
            (Some(why), _) => format!("Not read: {why}"),
            (None, true) => "Reading the registry\u{2019}s records\u{2026}".to_owned(),
            // A bounded read found none, and older records are left to look at.
            (None, false) if registry.more() => {
                "No record the filters keep among the newest read".to_owned()
            }
            (None, false) if registry.filter != Filter::default() => {
                "No record the filters keep".to_owned()
            }
            (None, false) => "No records".to_owned(),
        }));
    } else if let Some(why) = &registry.why {
        out.push(Line::Note(format!("Not read again: {why}")));
    }
    let names: BTreeMap<&str, BTreeMap<&str, &str>> = app
        .snapshot
        .iter()
        .flat_map(|snapshot| &snapshot.sheets)
        .map(|sheet| (sheet.name, super::names(sheet)))
        .collect();
    // A table the rail does not list is said as History's last page names it.
    let title = |name: &str| {
        app.title_of(name).map_or_else(
            || {
                registry
                    .tables
                    .iter()
                    .find(|(each, _)| each == name)
                    .map_or_else(|| super::super::sentence(name), |(_, title)| title.clone())
            },
            str::to_owned,
        )
    };
    let row = |collection: &str, pk: &str| {
        names
            .get(collection)
            .and_then(|rows| rows.get(pk))
            .map_or_else(|| pk.to_owned(), |words| (*words).to_owned())
    };
    let as_of = app.as_of().map(|at| at.operation);
    let claims: Vec<u64> = registry.records.iter().map(|r| r.by.at_ms).collect();
    for (range, head) in groups(&registry.records) {
        if let Some(head) = head {
            out.push(Line::Day(head));
        }
        let at = range.start;
        let run = &registry.records[range];
        let first = &run[0];
        let folds = run.len() >= RUN;
        let oldest = run[run.len() - 1].operation;
        let open = open(run, runs);
        if folds {
            out.push(Line::Run {
                oldest,
                by: first.by.clone(),
                records: run.len(),
                newest_ms: first.by.hlc.0,
                oldest_ms: run[run.len() - 1].by.hlc.0,
                open,
                ringed: !open
                    && registry
                        .selected
                        .is_some_and(|selected| run.iter().any(|r| r.operation == selected)),
            });
        }
        if !folds || open {
            for (n, record) in run.iter().enumerate() {
                let (place, changed) = place_words(record, title, row);
                let selected = registry.selected == Some(record.operation);
                out.push(Line::Record {
                    operation: record.operation,
                    by: record.by.clone(),
                    place,
                    changed,
                    cells: record.cells.len(),
                    inert: record.inert,
                    here: as_of == Some(record.operation),
                    ringed: selected,
                    apart: apart(&claims, at + n),
                    within: folds,
                });
                if selected {
                    let shows = |collection: &str| app.title_of(collection).is_some();
                    opened(&mut out, registry, record, title, row, shows);
                }
            }
        }
    }
    if registry.more() {
        out.push(Line::More);
    } else if !registry.records.is_empty() {
        out.push(Line::Note(
            "Start of the registry\u{2019}s records".to_owned(),
        ));
    }
    out
}

/// The selected `record`'s cells, opened under its line: for each, what it wrote and what that
/// replaced, then the writes beside it it had not seen and what has written there since, each a
/// line that leads to its row.
fn opened(
    out: &mut Vec<Line>,
    registry: &Registry,
    record: &Logged,
    title: impl Fn(&str) -> String,
    row: impl Fn(&str, &str) -> String,
    shows: impl Fn(&str) -> bool,
) {
    if record.inert {
        out.push(Line::Note(
            "Not counted: this record projects nothing".to_owned(),
        ));
    }
    let opened = match &registry.opened {
        None => {
            out.push(Line::Note(
                "Reading what this record wrote\u{2026}".to_owned(),
            ));
            return;
        }
        Some(Err(why)) => {
            out.push(Line::Note(format!("Not read: {why}")));
            return;
        }
        Some(Ok(opened)) => opened,
    };
    if opened.cells.is_empty() {
        out.push(Line::Note("It wrote no cell a reader keeps".to_owned()));
    }
    let values = |held: &[Valued]| {
        held.iter()
            .map(|value| format!("{} ({})", value.words, value.by))
            .collect::<Vec<_>>()
            .join(" / ")
    };
    for (n, cell) in opened.cells.iter().enumerate() {
        let at = format!(
            "{} \u{b7} {} \u{b7} {}",
            title(&cell.collection),
            row(&cell.collection, &cell.pk),
            cell.what
        );
        let was = if cell.was.is_empty() {
            "nothing".to_owned()
        } else {
            values(&cell.was)
        };
        let mut parts = vec![(Part::Was, format!("{at}: was {was}, now {}", cell.now))];
        if !cell.unseen.is_empty() {
            parts.push((
                Part::Unseen,
                format!("{at}: had not seen {}", values(&cell.unseen)),
            ));
        }
        if !cell.since.is_empty() {
            parts.push((
                Part::Since,
                format!("{at}: written since {}", values(&cell.since)),
            ));
        }
        let shows = shows(&cell.collection);
        for (part, words) in parts {
            out.push(Line::Wrote {
                operation: record.operation,
                n,
                part,
                words,
                shows,
            });
        }
    }
}

/// Which of an opened cell's lines: what it wrote over what it replaced, what it had not seen,
/// what has written there since.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Part {
    Was,
    Unseen,
    Since,
}

impl Part {
    pub(crate) const fn slug(self) -> &'static str {
        match self {
            Self::Was => "was",
            Self::Unseen => "unseen",
            Self::Since => "since",
        }
    }
}

/// The scope's head words: how many records are listed and whether older ones are left.
pub(crate) fn count_words(registry: &Registry) -> String {
    let n = registry.records.len();
    let records = match n {
        // Older records are left that a bounded read did not reach.
        0 if registry.more() => return "No record kept yet".to_owned(),
        1 => "1 record".to_owned(),
        n => format!("{n} records"),
    };
    if registry.more() {
        format!("Newest {records}")
    } else {
        records
    }
}

impl Scope {
    /// Whether the scope lists the registry's records rather than the shown table's.
    pub(crate) const fn registry(self) -> bool {
        matches!(self, Self::Registry)
    }
}

#[cfg(all(test, unix))]
#[path = "native_tables_history_registry_tests.rs"]
mod native_tables_history_registry_tests;
