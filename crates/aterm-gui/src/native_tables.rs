// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Tables page (docs/PLAN.md LT-08): the registry's stored tables, each a grid of its live
//! rows, the owner's edits staged and written in one Write through the same save
//! `aterm db table apply` makes ([`crate::tables_store`]), and a row's fields with their writers
//! and its history in the pane beside the grid.
//!
//! - **Rail.** One line per table: its name and its rows; the cells of it not yet written, where
//!   there are any.
//! - **Grid.** Columns under STORED (the owner's), SET BY ATERM (cells aterm keeps) and LINKED
//!   ROWS (how many rows of another table name this one). A stored cell is edited in place: a
//!   press selects it, a second press opens it, Return stages it, Escape leaves it.
//! - **Blue** means only "not yet written": a staged cell, the rail's count of them, the Write
//!   bar. Nothing else on the page is blue.
//! - **A conflict** shows every value, sorted by its words, and the pane lists each with its
//!   writer and a Keep; no clock picks one.
//! - **Secrets** never reach the page: a secret cell shows the grid's mask, and the pane and
//!   history say only whether it holds one.
//!
//! The page reads and writes on its own thread; a read answers in a [`Reply`] the host polls on
//! [`crate::Wake::TablesChanged`].

use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::mpsc;
use std::time::Duration;

use aterm_db::typed::grid::{Base, Seen};

use crate::native_app::{
    AppDescriptor, AppEvent, AppIcon, AppIndicators, AppKind, AppPresentation, CloseReadiness,
    CloseRequest, Command, CommonViewState, DamageRegion, EventResult, NativeAppModel,
    SemanticInput, TextInputEvent, UpdateCx, ViewCx,
};
use crate::native_text_input::TextInputState;
use crate::native_ui::{
    ActionId, ButtonSpec, Control, ControlState, GroupSpec, Insets, Layout, Length, SemanticRole,
    SemanticValue, StyleRef, TextFieldSpec, TextSpec, UiContent, UiKey, UiNode, UiTree,
};
use crate::native_workers::{one_line, text_width};

/// The page's name, on its tab and its heading.
pub(crate) const TABLES_TITLE: &str = "Tables";

/// How often an open page reads the registry again, for what other Macs and verbs wrote.
const READ_AGAIN: Duration = Duration::from_secs(30);

/// A column's group, in the grid's order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Group {
    /// The owner's cells: the only ones the page edits.
    Stored,
    /// Cells aterm keeps: set when a row is born, or by its own Mac.
    SetByAterm,
    /// How many rows of another table name this row.
    Linked,
}

impl Group {
    const fn word(self) -> &'static str {
        match self {
            Self::Stored => "STORED",
            Self::SetByAterm => "SET BY ATERM",
            Self::Linked => "LINKED ROWS",
        }
    }
}

/// Who wrote an operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(
    not(unix),
    expect(
        dead_code,
        reason = "only the registry read of macOS and Linux names a writer"
    )
)]
pub(crate) enum AuthorKind {
    Person,
    Aterm,
    Import,
}

/// An operation's writer: what kind, from which Mac, and the time its writer claimed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Author {
    pub(crate) kind: AuthorKind,
    /// The writing Mac's label, or the start of its machine id where no label is known.
    pub(crate) machine: Option<String>,
    /// The time the writer's clock claimed, signed with the operation.
    pub(crate) at_ms: u64,
    /// The operation's place in the causal order: the latest claim among it and everything it
    /// saw, and a count that steps past ancestors with that same claim (`Handle::history`'s hlc).
    pub(crate) hlc: (u64, u64),
}

impl Author {
    /// When the history places the operation, and the time its writer's clock claimed.
    fn when(&self, now: u64) -> String {
        let (ms, counter) = self.hlc;
        let mut words = when_words(ms, now);
        if counter > 0 {
            words.push_str(&format!(" +{counter}"));
        }
        format!("{words} \u{b7} claimed {}", when_words(self.at_ms, now))
    }
}

impl Author {
    /// `m73`, `aterm · m73`, `import`.
    pub(crate) fn who(&self) -> String {
        let machine = self.machine.as_deref();
        match (self.kind, machine) {
            (AuthorKind::Import, _) => "import".into(),
            (AuthorKind::Aterm, Some(m)) => format!("aterm \u{b7} {m}"),
            (AuthorKind::Aterm, None) => "aterm".into(),
            (AuthorKind::Person, Some(m)) => m.to_owned(),
            (AuthorKind::Person, None) => "a person".into(),
        }
    }
}

/// One cell as the page shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Shown {
    /// The grid's text: a conflict's values joined by ` / `.
    pub(crate) text: String,
    /// The writer of its one value.
    pub(crate) by: Option<Author>,
    /// A conflict's values, each with its writer, sorted by their words; empty for one value.
    pub(crate) values: Vec<(String, Option<Author>)>,
    /// A value this build cannot read: never edited here.
    pub(crate) raw: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SheetColumn {
    pub(crate) name: String,
    pub(crate) label: String,
    pub(crate) group: Group,
    pub(crate) editable: bool,
    pub(crate) numeric: bool,
    pub(crate) secret: bool,
    /// The grid masks this column's values (addresses): an edit starts empty.
    pub(crate) masked: bool,
    /// The table a reference or a linked count names.
    pub(crate) links: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SheetRow {
    pub(crate) pk: String,
    pub(crate) cells: BTreeMap<String, Shown>,
}

/// One cell an operation set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WrittenCell {
    pub(crate) pk: String,
    pub(crate) column: String,
    pub(crate) secret: bool,
    /// The value it set, as the grid shows it; empty for a cleared cell.
    pub(crate) now: String,
}

/// One operation on a table: one Write, however many cells it set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Written {
    pub(crate) by: Author,
    pub(crate) cells: Vec<WrittenCell>,
}

/// One collection as read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Sheet {
    pub(crate) name: &'static str,
    pub(crate) title: &'static str,
    pub(crate) singular: &'static str,
    pub(crate) columns: Vec<SheetColumn>,
    pub(crate) rows: Vec<SheetRow>,
    /// Its operations, oldest first.
    pub(crate) written: Vec<Written>,
    /// The export the grid shows: what a Write is compared against.
    pub(crate) base: Base,
}

/// Every shown collection, as one read found them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Snapshot {
    pub(crate) sheets: Vec<Sheet>,
    pub(crate) read_ms: u64,
}

/// One collection's staged cells and the export they were staged against.
pub(crate) struct Staged {
    pub(crate) base: Base,
    pub(crate) cells: BTreeMap<(String, String), String>,
}

/// What one Write did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Wrote {
    pub(crate) cells: usize,
    /// Cells another writer changed since the read: kept staged, never written over.
    pub(crate) kept: Vec<(String, String, String)>,
    /// Cells the table's rules or types refused, with the rule; nothing of that table was written.
    pub(crate) refused: Vec<(String, String, String, String)>,
    /// Tables whose save did not reach the registry, and why; nothing of them was written.
    pub(crate) failed: Vec<(String, String)>,
}

enum Work {
    Read,
    Write(Vec<Staged>),
}

pub(crate) enum Reply {
    Read(Result<Snapshot, String>),
    Wrote(Result<Wrote, String>),
}

/// A staged cell's address: collection, row, column.
type At = (String, String, String);

pub(crate) struct TablesApp {
    work: mpsc::Sender<Work>,
    replies: mpsc::Receiver<Reply>,
    snapshot: Option<Snapshot>,
    /// Why the last read found nothing, while no read has.
    unread: Option<String>,
    writing: bool,
    /// What the last Write did, until the next edit.
    note: Option<String>,
    staged: BTreeMap<At, String>,
    /// What each staged cell held in the read it was staged on: a Write is compared against that,
    /// so a value another writer wrote since is never written over.
    seen: BTreeMap<At, Seen>,
    /// The cells the Write in flight carried, as it carried them.
    sent: BTreeMap<At, String>,
    /// The rule a Write was refused by, at its cell.
    refused: BTreeMap<At, String>,
}

impl TablesApp {
    /// The page's model, reading the registry at `paths` (this Mac's, where `None`) on its own
    /// thread, which calls `wake` after each answer.
    ///
    /// # Errors
    /// The thread cannot start.
    pub(crate) fn new(
        paths: Option<crate::tables_store::Paths>,
        wake: impl Fn() + Send + 'static,
    ) -> Result<Self, String> {
        let (work_tx, work_rx) = mpsc::channel::<Work>();
        let (reply_tx, reply_rx) = mpsc::channel();
        crate::qos::spawn_lane_thread("aterm-tables", crate::qos::Role::Background, move || {
            let paths = match paths {
                Some(paths) => Ok(paths),
                None => crate::tables_store::live(),
            };
            loop {
                let work = match work_rx.recv_timeout(READ_AGAIN) {
                    Ok(work) => work,
                    Err(mpsc::RecvTimeoutError::Timeout) => Work::Read,
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                };
                let paths = match &paths {
                    Ok(paths) => paths,
                    Err(why) => {
                        if reply_tx.send(Reply::Read(Err(why.clone()))).is_err() {
                            return;
                        }
                        wake();
                        continue;
                    }
                };
                if let Work::Write(staged) = work {
                    let wrote = crate::tables_store::write(paths, staged);
                    if reply_tx.send(Reply::Wrote(wrote)).is_err() {
                        return;
                    }
                }
                let read = crate::tables_store::read(paths);
                if reply_tx.send(Reply::Read(read)).is_err() {
                    return;
                }
                wake();
            }
        })
        .map_err(|e| format!("the Tables reader could not start: {e}"))?;
        let app = Self {
            work: work_tx,
            replies: reply_rx,
            snapshot: None,
            unread: None,
            writing: false,
            note: None,
            staged: BTreeMap::new(),
            seen: BTreeMap::new(),
            sent: BTreeMap::new(),
            refused: BTreeMap::new(),
        };
        let _ = app.work.send(Work::Read);
        Ok(app)
    }

    /// Take every answer the reader sent; whether the page changed.
    pub(crate) fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(reply) = self.replies.try_recv() {
            self.take(reply);
            changed = true;
        }
        changed
    }

    /// Wait up to a minute for the reader's next answer and take it: a test's hang detector.
    #[cfg(all(test, unix))]
    pub(crate) fn settle(&mut self) {
        let reply = self
            .replies
            .recv_timeout(Duration::from_secs(60))
            .expect("the Tables reader answers within a minute");
        self.take(reply);
    }

    /// Take answers until the Write's and the read after it: a test's hang detector.
    #[cfg(all(test, unix))]
    pub(crate) fn settle_write(&mut self) {
        let mut wrote = false;
        loop {
            let reply = self
                .replies
                .recv_timeout(Duration::from_secs(60))
                .expect("the Tables reader answers within a minute");
            let (is_wrote, is_read) = (
                matches!(reply, Reply::Wrote(_)),
                matches!(reply, Reply::Read(_)),
            );
            self.take(reply);
            if wrote && is_read {
                return;
            }
            wrote |= is_wrote;
        }
    }

    /// Take `snapshot` as if the reader had read it.
    #[cfg(all(test, unix))]
    pub(crate) fn show(&mut self, snapshot: Snapshot) {
        self.take(Reply::Read(Ok(snapshot)));
    }

    fn take(&mut self, reply: Reply) {
        match reply {
            Reply::Read(Ok(snapshot)) => {
                self.snapshot = Some(snapshot);
                self.unread = None;
            }
            Reply::Read(Err(why)) => {
                if self.snapshot.is_none() {
                    self.unread = Some(why);
                }
            }
            Reply::Wrote(wrote) => {
                self.writing = false;
                match wrote {
                    Ok(wrote) => self.wrote(wrote),
                    Err(why) => self.note = Some(format!("Not written: {why}")),
                }
                self.sent.clear();
            }
        }
    }

    fn wrote(&mut self, wrote: Wrote) {
        let held: Vec<At> = wrote
            .kept
            .iter()
            .cloned()
            .chain(
                wrote
                    .refused
                    .iter()
                    .map(|(c, p, k, _)| (c.clone(), p.clone(), k.clone())),
            )
            .collect();
        let unwritten: Vec<&String> = wrote
            .refused
            .iter()
            .map(|(c, ..)| c)
            .chain(wrote.failed.iter().map(|(c, _)| c))
            .collect();
        // A refused or failed table wrote nothing: all its staged cells stay. Elsewhere a cell
        // stays when it was kept, or when it was staged or changed after the Write left.
        let sent = std::mem::take(&mut self.sent);
        self.staged.retain(|at, text| {
            held.contains(at) || unwritten.contains(&&at.0) || sent.get(at) != Some(text)
        });
        let staged = &self.staged;
        self.seen.retain(|at, _| staged.contains_key(at));
        self.refused = wrote
            .refused
            .iter()
            .map(|(c, p, k, rule)| ((c.clone(), p.clone(), k.clone()), rule.clone()))
            .collect();
        let mut words = vec![match wrote.cells {
            1 => "Wrote 1 cell".to_owned(),
            n => format!("Wrote {n} cells"),
        }];
        if !wrote.kept.is_empty() {
            words.push(format!(
                "{} changed elsewhere since the read, kept here",
                wrote.kept.len()
            ));
        }
        if let Some((_, _, column, rule)) = wrote.refused.first() {
            words.push(format!("{}: {rule}", crate::tables_store::label_of(column)));
        }
        for (table, why) in &wrote.failed {
            let title = crate::tables_store::SHOWN
                .iter()
                .find(|(name, ..)| name == table)
                .map_or(table.as_str(), |(_, title, _)| title);
            words.push(format!("{title} not written: {why}"));
        }
        self.note = Some(words.join(" \u{b7} "));
    }

    fn sheet(&self, name: &str) -> Option<&Sheet> {
        self.snapshot
            .as_ref()
            .and_then(|s| s.sheets.iter().find(|sheet| sheet.name == name))
    }

    fn staged_in(&self, sheet: &str) -> usize {
        self.staged.keys().filter(|(c, ..)| c == sheet).count()
    }

    /// Stage `text` in a cell, or take back what was staged where it is the written value.
    fn stage(&mut self, sheet: &str, pk: &str, column: &str, text: String) {
        let at = (sheet.to_owned(), pk.to_owned(), column.to_owned());
        let shown = self
            .sheet(sheet)
            .and_then(|s| s.rows.iter().find(|r| r.pk == pk))
            .and_then(|r| r.cells.get(column));
        let written = shown.is_some_and(|c| c.values.is_empty() && c.text == text);
        self.refused.remove(&at);
        self.note = None;
        if written {
            self.unstage(&at);
        } else {
            self.put(at, text);
        }
    }

    /// Stage `text` at `at`, remembering what the cell held in this read the first time.
    fn put(&mut self, at: At, text: String) {
        if !self.seen.contains_key(&at)
            && let Some(seen) = self
                .sheet(&at.0)
                .and_then(|s| s.base.rows.iter().find(|(pk, _)| *pk == at.1))
                .and_then(|(_, row)| row.get(&at.2))
                .cloned()
        {
            self.seen.insert(at.clone(), seen);
        }
        self.staged.insert(at, text);
    }

    fn unstage(&mut self, at: &At) {
        self.staged.remove(at);
        self.seen.remove(at);
        self.refused.remove(at);
    }

    /// What a staged cell held when it was staged, where another writer changed it since.
    fn written_meanwhile(&self, at: &At) -> Option<String> {
        let then = self.seen.get(at)?;
        let now = self
            .sheet(&at.0)?
            .base
            .rows
            .iter()
            .find(|(pk, _)| *pk == at.1)?
            .1
            .get(&at.2)?;
        (now.candidates != then.candidates || now.elements != then.elements)
            .then(|| now.shown.clone())
    }

    /// Whether Keep may stage one of a conflicted cell's values: a cell the page writes, whose
    /// values it shows as they are stored.
    fn keepable(&self, sheet: &str, pk: &str, column: &str) -> bool {
        let Some(sheet) = self.sheet(sheet) else {
            return false;
        };
        sheet
            .columns
            .iter()
            .find(|c| c.name == column)
            .is_some_and(|c| c.editable && !c.secret && !c.masked)
            && sheet
                .rows
                .iter()
                .find(|r| r.pk == pk)
                .and_then(|r| r.cells.get(column))
                .is_some_and(|c| !c.raw)
    }

    fn write(&mut self) {
        if self.staged.is_empty() || self.writing {
            return;
        }
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let mut staged: Vec<Staged> = Vec::new();
        let mut sent = BTreeMap::new();
        for (at, text) in &self.staged {
            let (collection, pk, column) = at;
            let Some(sheet) = snapshot.sheets.iter().find(|s| s.name == collection) else {
                continue;
            };
            let index = match staged.iter().position(|s| s.base.collection == *collection) {
                Some(index) => index,
                None => {
                    staged.push(Staged {
                        base: sheet.base.clone(),
                        cells: BTreeMap::new(),
                    });
                    staged.len() - 1
                }
            };
            // The cell is compared against what it held when it was staged, not this read.
            let Some(cell) = staged[index]
                .base
                .rows
                .iter_mut()
                .find(|(row, _)| row == pk)
                .and_then(|(_, row)| row.get_mut(column))
            else {
                continue;
            };
            if let Some(then) = self.seen.get(at) {
                cell.clone_from(then);
            }
            staged[index]
                .cells
                .insert((pk.clone(), column.clone()), text.clone());
            sent.insert(at.clone(), text.clone());
        }
        if self.work.send(Work::Write(staged)).is_ok() {
            self.writing = true;
            self.sent = sent;
            self.note = None;
        }
    }
}

/// The cell being edited and what it held when opened.
pub(crate) struct Editing {
    pk: String,
    column: String,
    seed: String,
    input: TextInputState,
}

pub(crate) struct TablesViewState {
    pub(crate) common: CommonViewState,
    sheet: &'static str,
    row: Option<String>,
    column: Option<String>,
    editing: Option<Editing>,
    find: TextInputState,
    scroll: f32,
    across: f32,
    pane_scroll: f32,
    limit: Cell<f32>,
    room: Cell<f32>,
    pane_limit: Cell<f32>,
    /// The grid's room across, and where each column of the shown sheet starts and ends.
    grid_room: Cell<f32>,
    spans: std::cell::RefCell<Vec<(String, f32, f32)>>,
    /// The rows the grid listed last, in order.
    listed: std::cell::RefCell<Vec<String>>,
}

impl Default for TablesViewState {
    fn default() -> Self {
        Self {
            common: CommonViewState::default(),
            sheet: crate::tables_store::SHOWN[0].0,
            row: None,
            column: None,
            editing: None,
            find: TextInputState::new(String::new()),
            scroll: 0.0,
            across: 0.0,
            pane_scroll: 0.0,
            limit: Cell::new(0.0),
            room: Cell::new(0.0),
            pane_limit: Cell::new(0.0),
            grid_room: Cell::new(0.0),
            spans: std::cell::RefCell::new(Vec::new()),
            listed: std::cell::RefCell::new(Vec::new()),
        }
    }
}

const SHEET: &str = "tables/sheet/";
const CELL: &str = "tables/cell/";
const UNDO: &str = "tables/pane/undo/";
const KEEP: &str = "tables/pane/keep/";
const EDIT: &str = "tables/edit";
const FIND: &str = "tables/find";
const WRITE: &str = "tables/write";
const DISCARD: &str = "tables/discard";

impl TablesViewState {
    /// Scroll an `act` on a cell outside the grid's room into view; whether anything moved.
    pub(crate) fn reveal_listed_control(&mut self, key: &UiKey) -> bool {
        let Some((pk, column)) = key
            .as_str()
            .strip_prefix(CELL)
            .and_then(|rest| rest.split_once('/'))
        else {
            return false;
        };
        let Some(at) = self.listed.borrow().iter().position(|listed| listed == pk) else {
            return false;
        };
        let (scroll, across) = (self.scroll, self.across);
        let line = row_height();
        let top = at as f32 * line;
        let room = self.room.get();
        if top < self.scroll {
            self.scroll = top;
        } else if room > 0.0 && top + line > self.scroll + room {
            self.scroll = top + line - room;
        }
        let column = column.split('/').next().unwrap_or(column).to_owned();
        self.reveal(&column);
        scroll != self.scroll || across != self.across
    }

    fn focus(&mut self, key: &str) {
        self.common.last_focus = Some(UiKey::new(key.to_owned()));
    }

    /// The rows the find field leaves, in the sheet's order; a row with a staged cell stays.
    fn shown_rows<'a>(&self, app: &TablesApp, sheet: &'a Sheet) -> Vec<&'a SheetRow> {
        let find = self.find.value().trim().to_lowercase();
        sheet
            .rows
            .iter()
            .filter(|row| {
                find.is_empty()
                    || row.pk.starts_with(&find)
                    || app
                        .staged
                        .keys()
                        .any(|(c, p, _)| c == sheet.name && *p == row.pk)
                    || sheet.columns.iter().any(|column| {
                        !column.secret
                            && column.group != Group::Linked
                            && row
                                .cells
                                .get(&column.name)
                                .is_some_and(|c| c.text.to_lowercase().contains(&find))
                    })
            })
            .collect()
    }

    /// Close the open cell: stage what it holds unless `cancel` or it is what it opened with.
    fn close_edit(&mut self, app: &mut TablesApp, cancel: bool) {
        let Some(editing) = self.editing.take() else {
            return;
        };
        if !cancel && editing.input.value() != editing.seed {
            app.stage(
                self.sheet,
                &editing.pk,
                &editing.column,
                editing.input.value().to_owned(),
            );
        }
        self.focus(&cell_key(&editing.pk, &editing.column));
    }

    fn open_edit(&mut self, app: &TablesApp, pk: &str, column: &str) {
        let Some(sheet) = app.sheet(self.sheet) else {
            return;
        };
        let Some(col) = sheet.columns.iter().find(|c| c.name == column) else {
            return;
        };
        let shown = sheet
            .rows
            .iter()
            .find(|r| r.pk == pk)
            .and_then(|r| r.cells.get(column));
        if !col.editable || shown.is_some_and(|c| c.raw) {
            return;
        }
        let at = (self.sheet.to_owned(), pk.to_owned(), column.to_owned());
        let seed = app.staged.get(&at).cloned().unwrap_or_else(|| {
            if col.masked || col.secret || shown.is_some_and(|c| !c.values.is_empty()) {
                String::new()
            } else {
                shown.map(|c| c.text.clone()).unwrap_or_default()
            }
        });
        let mut input = TextInputState::new(seed.clone());
        input.select_all();
        self.editing = Some(Editing {
            pk: pk.to_owned(),
            column: column.to_owned(),
            seed,
            input,
        });
        self.focus(EDIT);
    }

    fn select(&mut self, pk: &str, column: &str) {
        self.row = Some(pk.to_owned());
        self.column = Some(column.to_owned());
        self.pane_scroll = 0.0;
        self.focus(&cell_key(pk, column));
        self.reveal(column);
    }

    /// Scroll across so `column` shows whole.
    fn reveal(&mut self, column: &str) {
        let spans = self.spans.borrow();
        let room = self.grid_room.get();
        if let Some((_, start, end)) = spans.iter().find(|(name, ..)| name == column)
            && room > 0.0
        {
            if *start < self.across {
                self.across = *start;
            } else if *end > self.across + room {
                self.across = (*end - room).max(0.0);
            }
        }
    }

    /// Move the selection `rows` down (up where negative), or across by `columns`.
    fn step(&mut self, app: &TablesApp, rows: i32, columns: i32) {
        let Some(sheet) = app.sheet(self.sheet) else {
            return;
        };
        let shown = self.shown_rows(app, sheet);
        if shown.is_empty() {
            return;
        }
        let at = self
            .row
            .as_ref()
            .and_then(|pk| shown.iter().position(|r| r.pk == *pk));
        let row = match at {
            None => 0,
            Some(at) => (at as i64 + i64::from(rows)).clamp(0, shown.len() as i64 - 1) as usize,
        };
        let names: Vec<&str> = ordered(sheet).iter().map(|c| c.name.as_str()).collect();
        let column_at = self
            .column
            .as_ref()
            .and_then(|c| names.iter().position(|n| n == c))
            .unwrap_or(0);
        let column = (column_at as i64 + i64::from(columns)).clamp(0, names.len() as i64 - 1);
        let pk = shown[row].pk.clone();
        let column = names[column as usize].to_owned();
        self.select(&pk, &column);
        let line = row_height();
        let top = row as f32 * line;
        let room = self.room.get();
        if top < self.scroll {
            self.scroll = top;
        } else if room > 0.0 && top + line > self.scroll + room {
            self.scroll = top + line - room;
        }
    }
}

fn cell_key(pk: &str, column: &str) -> String {
    format!("{CELL}{pk}/{column}")
}

/// A sheet's columns in the grid's order: STORED, then SET BY ATERM, then LINKED ROWS, each as
/// the schema declares them.
fn ordered(sheet: &Sheet) -> Vec<&SheetColumn> {
    let mut columns: Vec<&SheetColumn> = sheet.columns.iter().collect();
    columns.sort_by_key(|c| c.group);
    columns
}

fn edit(input: &mut TextInputState, event: TextInputEvent) {
    match event {
        TextInputEvent::Preedit { text, selection } => input.set_preedit(text, selection),
        TextInputEvent::Commit(text) => input.commit_preedit(&text),
        TextInputEvent::Backspace => input.delete_backward(),
        TextInputEvent::Delete => input.delete_forward(),
        TextInputEvent::Left { extend } => input.move_left(extend),
        TextInputEvent::Right { extend } => input.move_right(extend),
        TextInputEvent::Home { extend } => input.move_to_start(extend),
        TextInputEvent::End { extend } => input.move_to_end(extend),
        TextInputEvent::WordLeft { extend } => input.move_word_left(extend),
        TextInputEvent::WordRight { extend } => input.move_word_right(extend),
        TextInputEvent::KillToEnd => input.kill_to_end(),
        TextInputEvent::KillToStart => input.kill_to_start(),
        TextInputEvent::DeleteWordBackward => input.delete_word_backward(),
        TextInputEvent::DeleteWordForward => input.delete_word_forward(),
        TextInputEvent::SelectAll => input.select_all(),
        TextInputEvent::Undo => {
            input.undo();
        }
        TextInputEvent::Redo => {
            input.redo();
        }
        TextInputEvent::Submit | TextInputEvent::Cancel => input.cancel_preedit(),
    }
}

impl TablesApp {
    /// The page's reducer, apart from the runtime's bookkeeping.
    fn act(&mut self, view: &mut TablesViewState, event: AppEvent) -> bool {
        match event {
            #[cfg(any(a11y_tree, test))]
            AppEvent::RevealControl(key) => view.reveal_listed_control(&key),
            // Text an assistive technology inserts goes where typed text would.
            #[cfg(any(a11y_tree, test))]
            AppEvent::InsertText(text) => {
                self.act(view, AppEvent::TextInput(TextInputEvent::Commit(text)))
            }
            AppEvent::FocusChanged(focus) => {
                if view.editing.is_some() && focus.as_ref().is_none_or(|k| k.as_str() != EDIT) {
                    // Leaving an open cell keeps what was typed in it.
                    view.close_edit(self, false);
                }
                // A cell the keyboard or an assistive technology moved to is the selection.
                if let Some((pk, column)) = focus
                    .as_ref()
                    .and_then(|k| k.as_str().strip_prefix(CELL))
                    .and_then(|rest| rest.split_once('/'))
                {
                    view.row = Some(pk.to_owned());
                    view.column = Some(column.to_owned());
                }
                view.common.last_focus = focus;
                true
            }
            AppEvent::Action(action) => {
                let key = action.id.as_str().to_owned();
                if let Some(SemanticInput::Text(value)) = action.value.as_ref() {
                    if key == EDIT {
                        if let Some(editing) = view.editing.as_mut() {
                            editing.input = TextInputState::new(value.clone());
                        }
                        return true;
                    }
                    if key == FIND {
                        view.find = TextInputState::new(value.clone());
                        view.scroll = 0.0;
                        return true;
                    }
                }
                if let Some(SemanticInput::TextPosition { byte, extend }) = action.value.as_ref() {
                    let input = match key.as_str() {
                        EDIT => view.editing.as_mut().map(|e| &mut e.input),
                        FIND => Some(&mut view.find),
                        _ => None,
                    };
                    if let Some(input) = input {
                        let anchor = if *extend {
                            input.selection().anchor
                        } else {
                            *byte
                        };
                        input.set_selection(anchor, *byte);
                        return true;
                    }
                }
                if key == EDIT || key == FIND {
                    view.focus(&key);
                    return true;
                }
                if let Some(name) = key.strip_prefix(SHEET) {
                    view.close_edit(self, false);
                    if let Some((name, ..)) =
                        crate::tables_store::SHOWN.iter().find(|(n, ..)| *n == name)
                    {
                        if view.sheet != *name {
                            view.sheet = name;
                            view.row = None;
                            view.column = None;
                            view.scroll = 0.0;
                            view.across = 0.0;
                            view.pane_scroll = 0.0;
                        }
                        view.focus(&key);
                    }
                    return true;
                }
                if let Some(rest) = key.strip_prefix(CELL)
                    && let Some((pk, column)) = rest.split_once('/')
                {
                    let again = view.editing.is_none()
                        && view.row.as_deref() == Some(pk)
                        && view.column.as_deref() == Some(column);
                    view.close_edit(self, false);
                    if again {
                        view.open_edit(self, pk, column);
                    } else {
                        view.select(pk, column);
                    }
                    return true;
                }
                if let Some(column) = key.strip_prefix(UNDO) {
                    if let Some(pk) = view.row.clone() {
                        self.unstage(&(view.sheet.to_owned(), pk, column.to_owned()));
                    }
                    return true;
                }
                if let Some(rest) = key.strip_prefix(KEEP)
                    && let Some((column, n)) = rest.rsplit_once('/')
                {
                    let value = view.row.as_ref().and_then(|pk| {
                        self.sheet(view.sheet)?
                            .rows
                            .iter()
                            .find(|r| r.pk == *pk)?
                            .cells
                            .get(column)?
                            .values
                            .get(n.parse::<usize>().ok()?)
                            .map(|v| v.0.clone())
                    });
                    if let (Some(value), Some(pk)) = (value, view.row.clone())
                        && self.keepable(view.sheet, &pk, column)
                    {
                        self.refused.remove(&(
                            view.sheet.to_owned(),
                            pk.clone(),
                            column.to_owned(),
                        ));
                        self.put((view.sheet.to_owned(), pk, column.to_owned()), value);
                        self.note = None;
                    }
                    return true;
                }
                match key.as_str() {
                    // Find (Command-F, or a press on the field) puts the keyboard in it.
                    FIND => {
                        view.close_edit(self, false);
                        view.focus(FIND);
                        view.find.select_all();
                        true
                    }
                    WRITE => {
                        view.close_edit(self, false);
                        self.write();
                        true
                    }
                    DISCARD => {
                        view.editing = None;
                        self.staged.clear();
                        self.seen.clear();
                        self.refused.clear();
                        self.note = None;
                        true
                    }
                    _ => false,
                }
            }
            AppEvent::TextInput(event) => {
                let focus = view
                    .common
                    .last_focus
                    .as_ref()
                    .map(|k| k.as_str().to_owned());
                match focus.as_deref() {
                    Some(EDIT) if view.editing.is_some() => match event {
                        TextInputEvent::Submit => {
                            view.close_edit(self, false);
                            true
                        }
                        TextInputEvent::Cancel => {
                            view.close_edit(self, true);
                            true
                        }
                        event => {
                            if let Some(editing) = view.editing.as_mut() {
                                edit(&mut editing.input, event);
                            }
                            true
                        }
                    },
                    Some(FIND) => {
                        if matches!(event, TextInputEvent::Cancel) {
                            view.find = TextInputState::new(String::new());
                        } else if !matches!(event, TextInputEvent::Submit) {
                            edit(&mut view.find, event);
                        }
                        view.scroll = 0.0;
                        true
                    }
                    Some(key) if key.starts_with(CELL) => match event {
                        TextInputEvent::Left { .. } => {
                            view.step(self, 0, -1);
                            true
                        }
                        TextInputEvent::Right { .. } => {
                            view.step(self, 0, 1);
                            true
                        }
                        TextInputEvent::Submit => {
                            if let (Some(pk), Some(column)) =
                                (view.row.clone(), view.column.clone())
                            {
                                view.open_edit(self, &pk, &column);
                            }
                            true
                        }
                        // Typing on a selected cell opens it with what was typed.
                        TextInputEvent::Commit(text) if !text.is_empty() => {
                            if let (Some(pk), Some(column)) =
                                (view.row.clone(), view.column.clone())
                            {
                                view.open_edit(self, &pk, &column);
                                if let Some(editing) = view.editing.as_mut() {
                                    editing.input = TextInputState::new(text);
                                }
                            }
                            true
                        }
                        _ => false,
                    },
                    _ => false,
                }
            }
            AppEvent::StepFocus(rows) => {
                view.close_edit(self, false);
                view.step(self, rows, 0);
                true
            }
            AppEvent::ScrollAt { lines, key } if key.as_str().starts_with("tables/pane") => {
                view.pane_scroll = (view.pane_scroll + lines as f32 * row_height())
                    .clamp(0.0, view.pane_limit.get());
                true
            }
            AppEvent::ScrollLines(lines) | AppEvent::ScrollAt { lines, .. } => {
                view.scroll =
                    (view.scroll + lines as f32 * row_height()).clamp(0.0, view.limit.get());
                true
            }
            AppEvent::ScrollPage(pages) => {
                view.scroll = (view.scroll + pages as f32 * view.room.get().max(row_height()))
                    .clamp(0.0, view.limit.get());
                true
            }
            _ => false,
        }
    }

    /// Whether the page holds a typed text field the focus is in.
    pub(crate) fn field_input<'a>(
        view: &'a TablesViewState,
        key: &str,
    ) -> Option<&'a TextInputState> {
        match key {
            EDIT => view.editing.as_ref().map(|e| &e.input),
            FIND => Some(&view.find),
            _ => None,
        }
    }
}

impl NativeAppModel for TablesApp {
    type ViewState = TablesViewState;

    fn descriptor(&self) -> AppDescriptor {
        AppDescriptor {
            kind: AppKind::Tables,
            name: TABLES_TITLE,
            icon: AppIcon::Editor,
            singleton: true,
        }
    }

    fn update(
        &mut self,
        view: &mut Self::ViewState,
        event: AppEvent,
        cx: &mut UpdateCx<'_>,
    ) -> EventResult {
        if !self.act(view, event) {
            return EventResult::Bubble;
        }
        view.common.presentation_revision = view.common.presentation_revision.wrapping_add(1);
        cx.repaint(DamageRegion::All);
        EventResult::Handled
    }

    fn view(&self, view: &Self::ViewState, cx: &ViewCx<'_>) -> UiTree {
        page(self, view, cx.viewport.width, cx.viewport.height)
    }

    fn commands(&self, _: &Self::ViewState, _: &mut Vec<Command>) {}

    fn presentation(&self, _: &Self::ViewState) -> AppPresentation {
        AppPresentation {
            title: TABLES_TITLE.into(),
            icon: AppIcon::Editor,
            indicators: AppIndicators {
                dirty: !self.staged.is_empty(),
                busy: self.writing,
                attention: false,
            },
            closable: true,
            tooltip: Some(TABLES_TITLE.into()),
        }
    }

    fn prepare_close(&mut self, _: CloseRequest, _: &mut UpdateCx<'_>) -> CloseReadiness {
        // Closing would drop changes not yet written; the page offers to write them instead.
        if self.staged.is_empty() {
            return CloseReadiness::Ready;
        }
        let n = self.staged.len();
        CloseReadiness::Blocked {
            recovery: vec![Command {
                id: ActionId::new(WRITE),
                title: if n == 1 {
                    "Write 1 change".into()
                } else {
                    format!("Write {n} changes")
                },
                shortcut: None,
                enabled: !self.writing,
            }],
        }
    }
}

// --- the page -------------------------------------------------------------------------------

fn scale() -> f32 {
    crate::native_appearance::text_scale()
}

fn row_height() -> f32 {
    28.0 * scale().max(1.0)
}

fn group(key: impl Into<std::sync::Arc<str>>) -> UiNode {
    UiNode::new(
        key,
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
    )
}

fn styled(key: impl Into<std::sync::Arc<str>>, style: StyleRef) -> UiNode {
    UiNode::new(
        key,
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group).style(style)),
    )
}

fn words(
    key: impl Into<std::sync::Arc<str>>,
    text: String,
    style: StyleRef,
    width: Length,
) -> UiNode {
    UiNode::new(
        key,
        UiContent::Text(TextSpec {
            text,
            role: SemanticRole::Text,
            style,
        }),
    )
    .layout(Layout::default().width(width).height(Length::Fill))
}

/// Words on one line of `width`, ellipsised where they do not fit.
fn fitted(key: impl Into<std::sync::Arc<str>>, text: &str, style: StyleRef, width: f32) -> UiNode {
    words(
        key,
        one_line(text, SemanticRole::Text, style, width),
        style,
        Length::Fixed(width.max(0.0)),
    )
}

fn button(key: &str, label: &str, spoken: &str, style: StyleRef, enabled: bool) -> UiNode {
    let width = crate::native_ui::native_button_label_width(label) + 24.0;
    UiNode::new(
        key.to_owned(),
        UiContent::Button(
            Control::new(
                ButtonSpec::new(spoken.to_owned()).visual_label(label),
                ActionId::new(key.to_owned()),
            )
            .state(ControlState {
                enabled,
                ..ControlState::default()
            })
            .style(style),
        ),
    )
    .layout(
        Layout::default()
            .width(Length::Fixed(width))
            .height(Length::Fill),
    )
}

/// A press the size of its parent, under painted words: no label of its own.
fn press(key: &str, spoken: String) -> UiNode {
    UiNode::new(
        key.to_owned(),
        UiContent::Button(
            Control::new(
                ButtonSpec::new(spoken).visual_label(String::new()),
                ActionId::new(key.to_owned()),
            )
            .state(ControlState {
                enabled: true,
                ..ControlState::default()
            })
            .style(StyleRef::Navigation),
        ),
    )
}

fn field(
    key: &str,
    label: &str,
    input: &TextInputState,
    view: &TablesViewState,
    secret: bool,
) -> UiNode {
    UiNode::new(
        key.to_owned(),
        UiContent::TextField(
            Control::new(
                TextFieldSpec {
                    label: label.into(),
                    placeholder: Some(label.into()),
                    secret,
                    visual_value: None,
                    input: Some(input.projection()),
                    swatch: None,
                    swatch_line: false,
                    trailing_hint: None,
                    description: None,
                    end_button: false,
                },
                ActionId::new(key.to_owned()),
            )
            .value(SemanticValue::Text(if secret {
                String::new()
            } else {
                input.value().into()
            }))
            .state(ControlState {
                enabled: true,
                focused: view
                    .common
                    .last_focus
                    .as_ref()
                    .is_some_and(|k| k.as_str() == key),
                ..ControlState::default()
            })
            .style(StyleRef::Secondary),
        ),
    )
}

/// `n rows`, `1 row`, `n of m`.
fn rows_words(shown: usize, all: usize) -> String {
    if shown == all {
        match all {
            1 => "1 row".into(),
            n => format!("{n} rows"),
        }
    } else {
        format!("{shown} of {all}")
    }
}

/// When an operation's writer says it wrote: `just now`, or `Sat 7 Nov 19:12` on this Mac's clock.
pub(crate) fn when_words(at_ms: u64, now_ms: u64) -> String {
    if now_ms.saturating_sub(at_ms) < 60_000 {
        return "just now".into();
    }
    let unix = i64::try_from(at_ms / 1000).unwrap_or(i64::MAX);
    let local = unix + crate::presence::local_offset_at(unix).unwrap_or(0);
    let days = local.div_euclid(86_400);
    let of_day = local.rem_euclid(86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    const WEEKDAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    format!(
        "{} {day} {} {:02}:{:02}",
        WEEKDAYS[days.rem_euclid(7) as usize],
        MONTHS[(month - 1) as usize],
        of_day / 3600,
        (of_day % 3600) / 60
    )
}

/// How wide each column draws: its header or its widest value, within bounds.
fn widths(sheet: &Sheet, columns: &[&SheetColumn]) -> Vec<f32> {
    let pad = 20.0;
    columns
        .iter()
        .map(|column| {
            let header = text_width(&column.label, SemanticRole::Text, StyleRef::Note);
            let widest = sheet
                .rows
                .iter()
                .take(200)
                .filter_map(|row| row.cells.get(&column.name))
                .map(|cell| {
                    let tag = if cell.values.is_empty() { 0.0 } else { 70.0 };
                    text_width(&cell.text, SemanticRole::Text, StyleRef::Plain) + tag
                })
                .fold(0.0_f32, f32::max);
            (header.max(widest) + pad).clamp(56.0 * scale(), 260.0 * scale())
        })
        .collect()
}

fn rail(app: &TablesApp, view: &TablesViewState, width: f32) -> UiNode {
    let line = 30.0 * scale().max(1.0);
    let inner = width - 24.0;
    let mut children = vec![
        fitted("tables/rail/head", "STORED", StyleRef::Note, inner).layout(
            Layout::default()
                .width(Length::Fixed(inner))
                .height(Length::Fixed(24.0 * scale())),
        ),
    ];
    for (name, title, _) in crate::tables_store::SHOWN {
        let key = format!("{SHEET}{name}");
        let count = app.sheet(name).map(|s| s.rows.len());
        let staged = app.staged_in(name);
        let count_words = count.map_or_else(String::new, |n| n.to_string());
        let count_w = text_width(&count_words, SemanticRole::Text, StyleRef::Note);
        let staged_words = if staged == 0 {
            String::new()
        } else {
            staged.to_string()
        };
        let staged_w = if staged == 0 {
            0.0
        } else {
            text_width(&staged_words, SemanticRole::Text, StyleRef::Accent) + 4.0
        };
        let name_w = (inner - 20.0 - count_w - staged_w - 8.0).max(1.0);
        let mut painted = vec![fitted(
            format!("{key}/name"),
            title,
            if view.sheet == name {
                StyleRef::Primary
            } else {
                StyleRef::Plain
            },
            name_w,
        )];
        if staged > 0 {
            painted.push(words(
                format!("{key}/staged"),
                staged_words,
                StyleRef::Accent,
                Length::Fixed(staged_w),
            ));
        }
        painted.push(words(
            format!("{key}/count"),
            count_words,
            StyleRef::Note,
            Length::Fixed(count_w),
        ));
        let face = group(format!("{key}/face"))
            .layout(
                Layout::row()
                    .gap(4.0)
                    .padding(Insets::symmetric(10.0, 0.0))
                    .height(Length::Fixed(line)),
            )
            .children(painted)
            .paint_only_tree();
        let mut row = vec![];
        if view.sheet == name {
            row.push(styled(format!("{key}/band"), StyleRef::Code));
        }
        row.push(press(
            &key,
            format!(
                "{title}: {}{}",
                count.map_or_else(String::new, |n| rows_words(n, n)),
                if staged == 0 {
                    String::new()
                } else {
                    format!(", {staged} not written")
                }
            ),
        ));
        row.push(face);
        children.push(
            group(format!("{key}/seat"))
                .layout(Layout::default().height(Length::Fixed(line)))
                .children(row),
        );
    }
    if let Some(snapshot) = &app.snapshot {
        children.push(group("tables/rail/space").layout(Layout::default().height(Length::Fill)));
        children.push(
            fitted(
                "tables/rail/read",
                &format!(
                    "Read {}",
                    when_words(snapshot.read_ms, aterm_time::unix_millis().unwrap_or(0))
                ),
                StyleRef::Note,
                inner,
            )
            .layout(
                Layout::default()
                    .width(Length::Fixed(inner))
                    .height(Length::Fixed(20.0 * scale())),
            ),
        );
    }
    UiNode::new(
        "tables/rail",
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Navigation)),
    )
    .layout(
        Layout::column()
            .width(Length::Fixed(width))
            .padding(Insets::all(12.0))
            .gap(2.0),
    )
    .children(children)
}

/// `inner` set 10 points in from the left of a seat `width` wide.
fn padded(key: String, inner: UiNode, width: f32) -> UiNode {
    group(key)
        .layout(Layout::row().width(Length::Fixed(width)).padding(Insets {
            left: 10.0,
            ..Insets::default()
        }))
        .children(vec![inner])
}

/// The grid's lines: the group header, the column header and every row the find leaves.
#[expect(clippy::too_many_lines, reason = "one grid, laid out in one pass")]
fn grid(app: &TablesApp, view: &TablesViewState, sheet: &Sheet, width: f32, height: f32) -> UiNode {
    let columns = ordered(sheet);
    let widths = widths(sheet, &columns);
    let line = row_height();
    let head_line = 22.0 * scale().max(1.0);
    let total: f32 = widths.iter().sum();
    let room_across = (width - 2.0).max(1.0);
    view.grid_room.set(room_across);
    {
        let mut spans = view.spans.borrow_mut();
        spans.clear();
        let mut at = 0.0;
        for (column, w) in columns.iter().zip(&widths) {
            spans.push((column.name.clone(), at, at + w));
            at += w;
        }
    }
    let across = view.across.min((total - room_across).max(0.0));
    let lane = |key: String, children: Vec<UiNode>, h: f32| {
        group(key)
            .layout(
                Layout::row()
                    .height(Length::Fixed(h))
                    .clipped()
                    .scrolled(across),
            )
            .children(children)
    };
    // The group header: each group's word over its first column, its span ruled off.
    let mut groups = Vec::new();
    let mut at = 0;
    while at < columns.len() {
        let group_of = columns[at].group;
        let mut end = at;
        let mut span = 0.0;
        while end < columns.len() && columns[end].group == group_of {
            span += widths[end];
            end += 1;
        }
        let word = if group_of == Group::Linked && span < 96.0 {
            "LINKED"
        } else {
            group_of.word()
        };
        groups.push(padded(
            format!("tables/groups/{at}"),
            fitted(
                format!("tables/groups/{at}/word"),
                word,
                StyleRef::Note,
                span - 10.0,
            ),
            span,
        ));
        at = end;
    }
    let headers: Vec<UiNode> = columns
        .iter()
        .zip(&widths)
        .map(|(column, w)| {
            padded(
                format!("tables/head/{}", column.name),
                fitted(
                    format!("tables/head/{}/word", column.name),
                    &column.label,
                    StyleRef::Note,
                    w - 10.0,
                ),
                *w,
            )
        })
        .collect();
    let rows = view.shown_rows(app, sheet);
    *view.listed.borrow_mut() = rows.iter().map(|r| r.pk.clone()).collect();
    let room = (height - head_line - line - 8.0).max(0.0);
    let limit = (rows.len() as f32 * line - room).max(0.0);
    view.room.set(room);
    view.limit.set(limit);
    let mut lines = Vec::with_capacity(rows.len());
    for row in &rows {
        let selected = view.row.as_deref() == Some(row.pk.as_str());
        let mut cells = Vec::with_capacity(columns.len());
        for (column, w) in columns.iter().zip(&widths) {
            let key = cell_key(&row.pk, &column.name);
            let at = (sheet.name.to_owned(), row.pk.clone(), column.name.clone());
            let shown = row.cells.get(&column.name);
            let staged = app.staged.get(&at);
            let focused = selected && view.column.as_deref() == Some(column.name.as_str());
            let editing = view
                .editing
                .as_ref()
                .filter(|e| e.pk == row.pk && e.column == column.name);
            let mut seat = Vec::new();
            if let Some(editing) = editing {
                seat.push(
                    field(EDIT, &column.label, &editing.input, view, false).layout(
                        Layout::default()
                            .width(Length::Fixed(*w))
                            .height(Length::Fixed(line)),
                    ),
                );
            } else {
                if staged.is_some() {
                    seat.push(styled(format!("{key}/staged"), StyleRef::Primary));
                } else if focused {
                    seat.push(styled(format!("{key}/focus"), StyleRef::Secondary));
                }
                let text = staged
                    .cloned()
                    .unwrap_or_else(|| shown.map(|c| c.text.clone()).unwrap_or_default());
                let conflict = staged.is_none() && shown.is_some_and(|c| !c.values.is_empty());
                let mut spoken = format!(
                    "{} of {}: {}",
                    column.label,
                    row.pk,
                    if text.is_empty() { "empty" } else { &text }
                );
                if conflict {
                    spoken.push_str(&format!(", {} values", shown.map_or(0, |c| c.values.len())));
                }
                if staged.is_some() {
                    spoken.push_str(", not written");
                }
                seat.push(press(&key, spoken));
                let style = if column.group == Group::Stored {
                    StyleRef::Plain
                } else {
                    StyleRef::Secondary
                };
                let mut face = Vec::new();
                if conflict {
                    let n = shown.map_or(0, |c| c.values.len());
                    let tag = format!("{n} values");
                    let tag_w = text_width(&tag, SemanticRole::Text, StyleRef::Chip) + 6.0;
                    face.push(fitted(
                        format!("{key}/text"),
                        &text,
                        style,
                        (w - 20.0 - tag_w).max(1.0),
                    ));
                    face.push(words(
                        format!("{key}/tag"),
                        tag,
                        StyleRef::Chip,
                        Length::Fixed(tag_w),
                    ));
                } else if column.numeric {
                    let tw = text_width(&text, SemanticRole::Text, style).min(w - 20.0);
                    face.push(
                        group(format!("{key}/lead")).layout(Layout::default().width(Length::Fill)),
                    );
                    face.push(fitted(format!("{key}/text"), &text, style, tw));
                } else {
                    face.push(fitted(format!("{key}/text"), &text, style, w - 20.0));
                }
                seat.push(
                    group(format!("{key}/face"))
                        .layout(Layout::row().gap(4.0).padding(Insets::symmetric(10.0, 0.0)))
                        .children(face)
                        .paint_only_tree(),
                );
            }
            cells.push(
                group(format!("{key}/seat"))
                    .layout(
                        Layout::default()
                            .width(Length::Fixed(*w))
                            .height(Length::Fixed(line)),
                    )
                    .children(seat),
            );
        }
        let mut band = Vec::new();
        if selected {
            band.push(styled(
                format!("tables/row/{}/band", row.pk),
                StyleRef::Code,
            ));
        }
        band.push(lane(format!("tables/row/{}", row.pk), cells, line));
        lines.push(
            group(format!("tables/line/{}", row.pk))
                .layout(Layout::default().height(Length::Fixed(line)))
                .children(band),
        );
    }
    if rows.is_empty() {
        let words_now = if sheet.rows.is_empty() {
            "No rows"
        } else {
            "No row has these words"
        };
        lines.push(
            fitted(
                "tables/rows/none",
                words_now,
                StyleRef::Note,
                room_across - 20.0,
            )
            .layout(
                Layout::default()
                    .height(Length::Fixed(line))
                    .padding(Insets::symmetric(10.0, 0.0)),
            ),
        );
    }
    let body = group("tables/rows")
        .layout(
            Layout::column()
                .height(Length::Fixed(room))
                .clipped()
                .scrolled(view.scroll.min(limit)),
        )
        .room(room, limit > 0.0)
        .lists_every_row()
        .children(lines);
    styled("tables/grid", StyleRef::Secondary)
        .layout(
            Layout::column()
                .padding(Insets::symmetric(1.0, 4.0))
                .clipped(),
        )
        .children(vec![
            lane("tables/groups".into(), groups, head_line),
            lane("tables/head".into(), headers, line),
            body,
        ])
}

fn write_bar(app: &TablesApp, width: f32) -> Option<UiNode> {
    let n = app.staged.len();
    if n == 0 && app.note.is_none() {
        return None;
    }
    let h = 40.0 * scale().max(1.0);
    if n == 0 {
        let note = app.note.clone().unwrap_or_default();
        return Some(
            fitted("tables/note", &note, StyleRef::Note, width - 20.0).layout(
                Layout::default()
                    .height(Length::Fixed(h))
                    .padding(Insets::symmetric(10.0, 0.0)),
            ),
        );
    }
    let count = if n == 1 {
        "1 change not written".to_owned()
    } else {
        format!("{n} changes not written")
    };
    let discard = button(
        DISCARD,
        "Discard",
        "Discard every change not written",
        StyleRef::Secondary,
        !app.writing,
    );
    let write = button(
        WRITE,
        if app.writing {
            "Writing\u{2026}"
        } else {
            "Write"
        },
        &format!("Write {count}"),
        StyleRef::Primary,
        !app.writing,
    );
    let buttons_w = crate::native_ui::native_button_label_width("Discard")
        + crate::native_ui::native_button_label_width("Writing\u{2026}")
        + 48.0
        + 8.0;
    let count_w = text_width(&count, SemanticRole::Text, StyleRef::Primary);
    let note_w = (width - 28.0 - count_w - buttons_w - 24.0).max(0.0);
    let mut children = vec![fitted(
        "tables/bar/count",
        &count,
        StyleRef::Primary,
        count_w,
    )];
    children.push(fitted(
        "tables/bar/note",
        app.note.as_deref().unwrap_or_default(),
        StyleRef::Note,
        note_w,
    ));
    children.push(
        group("tables/bar/buttons")
            .layout(
                Layout::row()
                    .gap(8.0)
                    .width(Length::Fixed(buttons_w))
                    .padding(Insets::symmetric(0.0, 5.0)),
            )
            .children(vec![discard, write]),
    );
    Some(
        styled("tables/bar", StyleRef::Primary)
            .layout(
                Layout::row()
                    .gap(12.0)
                    .height(Length::Fixed(h))
                    .padding(Insets::symmetric(14.0, 0.0)),
            )
            .children(children),
    )
}

/// The pane's word for a value: `empty`, a secret only as `set`.
fn value_words(column: &SheetColumn, text: &str) -> String {
    if text.is_empty() {
        "empty".into()
    } else if column.secret {
        "set".into()
    } else {
        text.to_owned()
    }
}

/// A field's name in the pane: `Nickname`, `Ram gb`.
fn field_words(column: &SheetColumn) -> String {
    let lower = column.label.to_lowercase();
    let mut chars = lower.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
        .unwrap_or_default()
}

#[expect(clippy::too_many_lines, reason = "one pane, laid out in one pass")]
fn pane(app: &TablesApp, view: &TablesViewState, sheet: &Sheet, width: f32, height: f32) -> UiNode {
    let inner = width - 28.0;
    let line = 22.0 * scale().max(1.0);
    let mut children = Vec::new();
    let mut tall = 0.0;
    let mut push = |children: &mut Vec<UiNode>, node: UiNode, h: f32| {
        children.push(node.layout_height(h));
        tall += h + 4.0;
    };
    let Some(row) = view
        .row
        .as_ref()
        .and_then(|pk| sheet.rows.iter().find(|r| r.pk == *pk))
    else {
        push(
            &mut children,
            fitted(
                "tables/pane/none",
                "Select a row to see its fields and history",
                StyleRef::Note,
                inner,
            ),
            line,
        );
        return pane_frame(children, width, height, view, tall);
    };
    let columns = ordered(sheet);
    push(
        &mut children,
        fitted(
            "tables/pane/kind",
            &sheet.singular.to_uppercase(),
            StyleRef::Note,
            inner,
        ),
        line,
    );
    let name = columns
        .first()
        .and_then(|c| row.cells.get(&c.name))
        .map(|c| c.text.clone())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| row.pk.clone());
    push(
        &mut children,
        UiNode::new(
            "tables/pane/title",
            UiContent::Text(TextSpec::heading(one_line(
                &name,
                SemanticRole::Heading,
                StyleRef::Plain,
                inner,
            ))),
        ),
        30.0 * scale().max(1.0),
    );
    push(
        &mut children,
        fitted("tables/pane/id", &row.pk, StyleRef::Note, inner),
        line,
    );
    let now = aterm_time::unix_millis().unwrap_or(0);
    // Each conflicted cell: every value, its writer, and a Keep.
    for column in &columns {
        let Some(shown) = row.cells.get(&column.name).filter(|c| !c.values.is_empty()) else {
            continue;
        };
        let key = format!("tables/pane/conflict/{}", column.name);
        let mut boxed = vec![
            fitted(
                format!("{key}/head"),
                &format!("{} \u{b7} {} VALUES", column.label, shown.values.len()),
                StyleRef::Note,
                inner - 20.0,
            )
            .layout_height(line),
        ];
        let mut ordered_values: Vec<(usize, &(String, Option<Author>))> =
            shown.values.iter().enumerate().collect();
        ordered_values
            .sort_by_key(|(_, (_, by))| std::cmp::Reverse(by.as_ref().map_or(0, |b| b.at_ms)));
        for (n, (value, by)) in ordered_values {
            let who = by.as_ref().map_or_else(String::new, |by| {
                format!("{} \u{b7} {}", by.who(), when_words(by.at_ms, now))
            });
            // Keep stages a value, so it is offered only where the page writes the cell.
            let keep = app.keepable(sheet.name, &row.pk, &column.name).then(|| {
                button(
                    &format!("{KEEP}{}/{n}", column.name),
                    "Keep",
                    &format!(
                        "Keep {} for {}",
                        value_words(column, value),
                        field_words(column)
                    ),
                    StyleRef::Secondary,
                    true,
                )
            });
            let keep_w = crate::native_ui::native_button_label_width("Keep") + 24.0;
            boxed.push(
                group(format!("{key}/{n}"))
                    .layout(Layout::row().gap(8.0).height(Length::Fixed(line + 4.0)))
                    .children(
                        [
                            fitted(
                                format!("{key}/{n}/value"),
                                &value_words(column, value),
                                StyleRef::Plain,
                                (inner - 36.0 - keep_w) * 0.5,
                            ),
                            fitted(
                                format!("{key}/{n}/who"),
                                &who,
                                StyleRef::Note,
                                (inner - 36.0 - keep_w) * 0.5,
                            ),
                        ]
                        .into_iter()
                        .chain(keep)
                        .collect(),
                    ),
            );
        }
        let h = line + (shown.values.len() as f32) * (line + 8.0) + 20.0;
        push(
            &mut children,
            styled(key, StyleRef::Code)
                .layout(Layout::column().gap(4.0).padding(Insets::all(10.0)))
                .children(boxed),
            h,
        );
    }
    let label_w = (inner * 0.32).min(110.0 * scale());
    let meta_w = (inner * 0.26).min(96.0 * scale());
    let value_w = (inner - label_w - meta_w - 16.0).max(1.0);
    for group_of in [Group::Stored, Group::SetByAterm, Group::Linked] {
        let members: Vec<&&SheetColumn> = columns.iter().filter(|c| c.group == group_of).collect();
        if members.is_empty() {
            continue;
        }
        let head = format!("tables/pane/group/{}", group_of.word());
        let mut head_line = vec![fitted(
            format!("{head}/word"),
            group_of.word(),
            StyleRef::Note,
            inner - meta_w,
        )];
        if group_of == Group::Stored {
            head_line.push(fitted(
                format!("{head}/by"),
                "WRITTEN BY",
                StyleRef::Note,
                meta_w,
            ));
        }
        push(
            &mut children,
            group(head).layout(Layout::row()).children(head_line),
            line + 6.0,
        );
        let mut empty = Vec::new();
        for column in members {
            let key = format!("tables/pane/field/{}", column.name);
            let at = (sheet.name.to_owned(), row.pk.clone(), column.name.clone());
            let shown = row.cells.get(&column.name);
            let staged = app.staged.get(&at);
            if staged.is_none()
                && shown.is_none_or(|c| c.text.is_empty())
                && !app.refused.contains_key(&at)
            {
                empty.push(field_words(column));
                continue;
            }
            let text = staged
                .cloned()
                .unwrap_or_else(|| shown.map(|c| c.text.clone()).unwrap_or_default());
            let value_style = if staged.is_some() {
                StyleRef::Accent
            } else {
                StyleRef::Plain
            };
            let mut parts = vec![
                fitted(
                    format!("{key}/label"),
                    &field_words(column),
                    StyleRef::Note,
                    label_w,
                ),
                fitted(
                    format!("{key}/value"),
                    &value_words(column, &text),
                    value_style,
                    value_w,
                ),
            ];
            if staged.is_some() {
                parts.push(button(
                    &format!("{UNDO}{}", column.name),
                    "Undo",
                    &format!("Undo {}", field_words(column)),
                    StyleRef::Secondary,
                    true,
                ));
            } else if group_of == Group::Stored
                && let Some(by) = shown.and_then(|c| c.by.as_ref())
            {
                parts.push(fitted(
                    format!("{key}/by"),
                    &by.who(),
                    StyleRef::Note,
                    meta_w,
                ));
            }
            push(
                &mut children,
                group(key).layout(Layout::row().gap(8.0)).children(parts),
                line + 2.0,
            );
            if staged.is_some()
                && let Some(now) = app.written_meanwhile(&at)
            {
                push(
                    &mut children,
                    fitted(
                        format!("tables/pane/since/{}", column.name),
                        &format!("Written elsewhere since: {}", value_words(column, &now)),
                        StyleRef::Note,
                        inner,
                    ),
                    line,
                );
            }
            if let Some(refused) = app.refused.get(&at) {
                push(
                    &mut children,
                    fitted(
                        format!("tables/pane/refused/{}", column.name),
                        refused,
                        StyleRef::Danger,
                        inner,
                    ),
                    line,
                );
            }
        }
        if !empty.is_empty() {
            let words = format!("Empty: {}", empty.join(", "));
            let node = crate::native_about::wrapped(
                &format!("tables/pane/empty/{}", group_of.word()),
                &words,
                inner,
                SemanticRole::Text,
                StyleRef::Note,
            );
            let h = match node.layout.height {
                Length::Fixed(h) => h,
                _ => line,
            };
            push(&mut children, node, h);
        }
    }
    // History: newest first, one entry per operation that set a cell of this row.
    let history = row_history(sheet, &row.pk);
    if !history.is_empty() {
        push(
            &mut children,
            fitted("tables/pane/history", "HISTORY", StyleRef::Note, inner),
            line + 6.0,
        );
    }
    for (n, (by, changes)) in history.iter().enumerate().take(60) {
        let key = format!("tables/pane/written/{n}");
        push(
            &mut children,
            fitted(
                format!("{key}/who"),
                &format!("{} \u{b7} {}", by.who(), by.when(now)),
                StyleRef::Plain,
                inner,
            ),
            line,
        );
        for (m, change) in changes.iter().enumerate() {
            push(
                &mut children,
                fitted(format!("{key}/{m}"), change, StyleRef::Note, inner - 12.0).layout(
                    Layout::default().padding(Insets {
                        left: 12.0,
                        ..Insets::default()
                    }),
                ),
                line - 2.0,
            );
        }
    }
    pane_frame(children, width, height, view, tall)
}

/// A row's history, newest first: who, and each field it set as `Field: was → now`.
fn row_history(sheet: &Sheet, pk: &str) -> Vec<(Author, Vec<String>)> {
    let mut last: BTreeMap<&str, &str> = BTreeMap::new();
    let mut out = Vec::new();
    for written in &sheet.written {
        let mut lines = Vec::new();
        for cell in written.cells.iter().filter(|c| c.pk == pk) {
            let Some(column) = sheet.columns.iter().find(|c| c.name == cell.column) else {
                // The row's lifecycle and import columns are not shown; a birth reads as one.
                if cell.column == "row" && cell.now == "live" && !last.contains_key("row") {
                    lines.insert(0, "Row \u{2192} added".to_owned());
                    last.insert("row", "live");
                }
                continue;
            };
            let was = last.get(cell.column.as_str()).copied().unwrap_or("");
            let line = if column.secret {
                let word = match (was.is_empty(), cell.now.is_empty()) {
                    (true, false) => "set",
                    (false, true) => "cleared",
                    (false, false) => "changed",
                    (true, true) => continue,
                };
                format!("{}: {word}", field_words(column))
            } else if cell.now.starts_with("+ ") || cell.now.starts_with('\u{2212}') {
                format!("{}: {}", field_words(column), cell.now)
            } else {
                format!(
                    "{}: {} \u{2192} {}",
                    field_words(column),
                    value_words(column, was),
                    value_words(column, &cell.now)
                )
            };
            lines.push(line);
            last.insert(cell.column.as_str(), cell.now.as_str());
        }
        if !lines.is_empty() {
            out.push((written.by.clone(), lines));
        }
    }
    out.reverse();
    out
}

fn pane_frame(
    children: Vec<UiNode>,
    width: f32,
    height: f32,
    view: &TablesViewState,
    tall: f32,
) -> UiNode {
    let room = (height - 28.0).max(0.0);
    let limit = (tall - room).max(0.0);
    view.pane_limit.set(limit);
    styled("tables/pane", StyleRef::Secondary)
        .layout(
            Layout::column()
                .width(Length::Fixed(width))
                .padding(Insets::all(14.0))
                .clipped(),
        )
        .children(vec![
            group("tables/pane/list")
                .layout(
                    Layout::column()
                        .gap(4.0)
                        .height(Length::Fixed(room))
                        .clipped()
                        .scrolled(view.pane_scroll.min(limit)),
                )
                .room(room, limit > 0.0)
                .lists_every_row()
                .children(children),
        ])
}

fn page(app: &TablesApp, view: &TablesViewState, width: f32, height: f32) -> UiTree {
    let rail_w = 188.0_f32.min(width * 0.25);
    let pane_w = if width >= 900.0 {
        340.0_f32.min(width * 0.3)
    } else {
        0.0
    };
    let main_w = (width - rail_w - pane_w - 32.0 - if pane_w > 0.0 { 12.0 } else { 0.0 }).max(1.0);
    let line = 30.0 * scale().max(1.0);
    let mut main = Vec::new();
    let sheet = app.sheet(view.sheet);
    let (title, singular_count) = crate::tables_store::SHOWN
        .iter()
        .find(|(n, ..)| *n == view.sheet)
        .map_or((TABLES_TITLE, String::new()), |(_, title, _)| {
            (
                *title,
                sheet.map_or_else(String::new, |s| {
                    rows_words(view.shown_rows(app, s).len(), s.rows.len())
                }),
            )
        });
    let count_w = text_width(&singular_count, SemanticRole::Text, StyleRef::Note).min(main_w * 0.4);
    main.push(
        group("tables/header")
            .layout(
                Layout::row()
                    .gap(12.0)
                    .height(Length::Fixed(36.0 * scale().max(1.0))),
            )
            .children(vec![
                UiNode::new("tables/title", UiContent::Text(TextSpec::heading(title)))
                    .layout(Layout::default().width(Length::Fill)),
                fitted("tables/count", &singular_count, StyleRef::Note, count_w),
            ]),
    );
    main.push(
        group("tables/tools")
            .layout(Layout::row().height(Length::Fixed(line + 4.0)))
            .children(vec![field(FIND, "Find", &view.find, view, false).layout(
                Layout::default().width(Length::Fixed(220.0 * scale().max(1.0))),
            )]),
    );
    let bar = write_bar(app, main_w);
    let bar_h = if bar.is_some() {
        40.0 * scale().max(1.0) + 10.0
    } else {
        0.0
    };
    let grid_h =
        (height - 32.0 - 36.0 * scale().max(1.0) - line - 4.0 - 20.0 - bar_h).max(line * 3.0);
    match (sheet, &app.unread) {
        (Some(sheet), _) => {
            main.push(grid(app, view, sheet, main_w, grid_h).layout_height(grid_h));
        }
        (None, Some(why)) => main.push(crate::native_about::wrapped(
            "tables/unread",
            &format!("The tables could not be read here: {why}"),
            main_w,
            SemanticRole::Text,
            StyleRef::Note,
        )),
        (None, None) => main.push(
            fitted("tables/reading", "Reading\u{2026}", StyleRef::Note, main_w).layout_height(line),
        ),
    }
    if let Some(bar) = bar {
        main.push(bar);
    }
    let mut columns = vec![
        rail(app, view, rail_w),
        group("tables/main")
            .layout(
                Layout::column()
                    .gap(10.0)
                    .width(Length::Fixed(main_w))
                    .padding(Insets::all(16.0)),
            )
            .children(main),
    ];
    if pane_w > 0.0
        && let Some(sheet) = sheet
    {
        columns.push(
            group("tables/pane-seat")
                .layout(
                    Layout::column()
                        .width(Length::Fixed(pane_w + 12.0))
                        .padding(Insets {
                            top: 16.0,
                            bottom: 16.0,
                            right: 12.0,
                            left: 0.0,
                        }),
                )
                .children(vec![pane(app, view, sheet, pane_w, height - 32.0)]),
        );
    }
    let mut root = UiNode::new(
        "tables/root",
        UiContent::Group(GroupSpec::new(TABLES_TITLE)),
    )
    .layout(Layout::row().clipped())
    .children(columns);
    if let UiContent::Group(spec) = &mut root.content {
        spec.role = SemanticRole::Application;
    }
    UiTree::new(root)
}

trait FixedHeight {
    fn layout_height(self, h: f32) -> Self;
}

impl FixedHeight for UiNode {
    fn layout_height(mut self, h: f32) -> Self {
        self.layout.height = Length::Fixed(h);
        self
    }
}

#[cfg(all(test, unix))]
#[path = "native_tables_tests.rs"]
mod native_tables_tests;
