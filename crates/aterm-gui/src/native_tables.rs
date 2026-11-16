// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Tables page (docs/PLAN.md LT-08): the registry's stored tables, each a grid of its live
//! rows, the owner's edits staged and written in one Write through the same save
//! `aterm db table apply` makes ([`crate::tables_store`]), and a row's fields with their writers
//! and its history in the pane beside the grid.
//!
//! - **Rail.** One line per table: its name and its rows; the cells of it not yet written, where
//!   there are any.
//! - **Grid.** A sheet: ruled cells of one height under a header line of column names, each row
//!   numbered at its left. The header line stays at the top while the rows scroll under it, and
//!   the row numbers and the row's key column (`label`, an address's `addr`) stay at the left
//!   while the rest scroll across beside them (a sideways wheel or swipe, or a move of the
//!   selection); a thumb along the grid's foot says where they stand, and a bar marks the key
//!   column's edge while columns lie under it. A column is as wide as its header or its values
//!   need until a person drags its header's right edge; a double press on that edge, or its
//!   press from the keyboard, fits it to its widest value, and Left and Right on the focused
//!   edge size it. A press on a header orders the rows by its column, from its least value up,
//!   then from its greatest down, then as the registry holds them again; the header wears a
//!   drawn mark and says which. The order is the view's alone: it writes nothing and survives
//!   a new read. A row stands by the values the registry holds and is placed once for each
//!   read, find and sort, so typing into the sorted column moves no row: a staged cell takes
//!   its place when it is written and read back. Columns sit
//!   under STORED (the owner's), SET BY ATERM (cells aterm keeps) and LINKED ROWS (how many rows
//!   of another table name this one). A stored cell is edited in place: a press selects it, a
//!   second press opens it, Return stages it, Escape leaves it.
//! - **Keys** are a spreadsheet's. The arrows move the selection a cell; Tab and Shift-Tab move
//!   it a cell across and onto the next row, and off the sheet's last or first cell leave the
//!   grid as Tab leaves any control. Return opens the selected cell, and in an open one stages
//!   what was typed and moves down; Tab there stages and moves across; Escape leaves the cell
//!   as it was. A typed letter opens the cell with it. Delete or Backspace stages nothing in
//!   each selected cell that takes an edit. Shift and an arrow grow the selection into a range
//!   ([`StyleRef::CellRange`], each of its cells said selected) of the rows that lie between
//!   its corners then: once the rows shown or their order change it is the one selected cell
//!   again, as it is at Escape. A copy takes the selected cell's words, or the range's as
//!   tab-separated lines a spreadsheet reads back cell for cell, as the grid shows them: a
//!   secret's cell copies the entry name or mask it shows, never a value. A paste on a
//!   selected cell stages the clipboard's lines down the rows and their cells across the
//!   columns, each in a cell the page edits; in an open cell it is typed text. Save's key,
//!   File ▸ Save and the palette's Write press Write. After a pointer's press on a header or
//!   a drag of an edge the keys are the selected cell's again; pressed from the keyboard, a
//!   header keeps them.
//! - **Blue** means only "not yet written" ([`StyleRef::Unwritten`], the palette's one fixed
//!   [`crate::native_appearance::NOT_WRITTEN`], never the accent): a staged cell, the rail's
//!   count of them, the Write bar. Nothing else on the page wears it.
//! - **Writers** are named by their Mac's label (`m73`), resolved from what aterm holds; a writer
//!   none names shows the start of its machine id, dimmed.
//! - **A conflict** shows every value, sorted by its words, and the pane lists each with its
//!   writer and a Keep; no clock picks one. A Write is compared against what each staged cell
//!   held when the person last saw it: when it was staged, or when it was opened, since an
//!   open cell covers what a read brings.
//! - **Secrets** never reach the page: a secret cell names its secret-store entry, read-only here.
//!   Addresses and phone numbers are ordinary data and read as stored.
//! - **As of.** Each history entry offers the table as that operation's causal past holds it
//!   (`aterm db table get|scan|export --at`, the same projection), read only, each cell changed
//!   since marked in the grid and listed in the pane with what it holds now and who wrote that
//!   (`aterm db table diff <operation> head`); Now returns to the table as it is.
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
    /// The writing Mac's label (`m73`), where aterm knows one: its actor's seat, that seat's
    /// machine, that machine's label ([`crate::tables_store`]'s `Writers`).
    pub(crate) label: Option<String>,
    /// The start of the writer's Commons machine id (of its actor where none is known), which
    /// the page shows, dimmed, where no label is known.
    pub(crate) id: String,
    /// The time the writer's clock claimed, signed with the operation.
    pub(crate) at_ms: u64,
    /// The operation's place in the causal order: the latest claim among it and everything it
    /// saw, and a count that steps past ancestors with that same claim (`Handle::history`'s hlc).
    pub(crate) hlc: (u64, u64),
}

impl Author {
    /// When the history places the operation, and the time its writer's clock claimed.
    fn when(&self, now: u64) -> String {
        format!(
            "{} \u{b7} claimed {}",
            self.placed(now),
            when_words(self.at_ms, now)
        )
    }

    /// When the history places the operation: its causal clock's time, and its count past
    /// ancestors with that same time.
    fn placed(&self, now: u64) -> String {
        let (ms, counter) = self.hlc;
        let mut words = when_words(ms, now);
        if counter > 0 {
            words.push_str(&format!(" +{counter}"));
        }
        words
    }
}

impl Author {
    /// `m73`, `aterm · m73`, `import · m73`; the short id in a label's place where none is known.
    pub(crate) fn who(&self) -> String {
        let name = self.label.as_deref().unwrap_or(&self.id);
        match self.kind {
            AuthorKind::Import => format!("import \u{b7} {name}"),
            AuthorKind::Aterm => format!("aterm \u{b7} {name}"),
            AuthorKind::Person => name.to_owned(),
        }
    }

    /// How the writer's words are set: as `known` is where its Mac's label is known; where only
    /// its short id is, dimmed at `known`'s size.
    fn style(&self, known: StyleRef) -> StyleRef {
        match (&self.label, known) {
            (Some(_), _) => known,
            (None, StyleRef::Note) => StyleRef::FaintNote,
            (None, _) => StyleRef::Faint,
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

/// How a column's cells are read when its rows are put in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ColumnKind {
    /// A whole number: ordered by its value, and set at the right of its cell.
    Number,
    /// A time or a day, which the grid shows as its number: ordered by its value.
    Time,
    /// Words: ordered without regard to case, a run of digits in them by its value (`m9`
    /// before `m55`).
    Text,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SheetColumn {
    pub(crate) name: String,
    pub(crate) label: String,
    pub(crate) group: Group,
    pub(crate) editable: bool,
    pub(crate) kind: ColumnKind,
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
    /// The value it set, as the grid shows it; empty for a cleared cell.
    pub(crate) now: String,
}

/// One operation on a table: one Write, however many cells it set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Written {
    /// The operation's digest: the point an as-of read names.
    pub(crate) operation: [u8; 32],
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
    /// The column that names a row (`label`, an address's `addr`): the grid keeps it at its
    /// left while the rest scroll across.
    pub(crate) key: String,
    pub(crate) rows: Vec<SheetRow>,
    /// Its operations, oldest first.
    pub(crate) written: Vec<Written>,
    /// In a read as of a past point, each cell that changed since, by row and column.
    pub(crate) since: BTreeMap<(String, String), Since>,
    /// The export the grid shows: what a Write is compared against.
    pub(crate) base: Base,
}

/// A cell that changed after the point a past read is at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Since {
    /// What it holds now, as the pane shows it: empty for a cleared cell; a set's members put in
    /// (`+ x`) or taken out (`− x`).
    pub(crate) now: String,
    /// Who wrote what it holds now: each concurrent writer of a conflict, in causal order.
    pub(crate) by: Vec<Author>,
    /// `now` lists a set's members put in or taken out, not a value.
    pub(crate) members: bool,
}

/// The past point a read is at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AsOf {
    pub(crate) operation: [u8; 32],
    /// Its writer, where history names the operation.
    pub(crate) by: Option<Author>,
}

/// Every shown collection, as one read found them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Snapshot {
    pub(crate) sheets: Vec<Sheet>,
    /// The past point this read is at; `None` for the table as it is.
    pub(crate) at: Option<AsOf>,
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
    /// Read again at the point the reader is at.
    Read,
    /// Read as of an operation (the table as it is, where `None`), and stay there once read.
    At(Option<[u8; 32]>),
    Write(Vec<Staged>),
}

pub(crate) enum Reply {
    /// A read, and whether it answers a move to another point ([`Work::At`]).
    Read {
        read: Result<Snapshot, String>,
        moved: bool,
    },
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
    /// Moves to another point asked for that have not answered: the page takes no edit until
    /// the last of them has, since each answer may land it on a past table.
    asked: usize,
    /// How many reads the page has taken: a view works its rows' order out once for each
    /// ([`TablesViewState::shown_rows`]).
    reads: u64,
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
            // The point the page shows: a read at another point moves it only once it reads.
            let mut point = None;
            loop {
                let work = match work_rx.recv_timeout(READ_AGAIN) {
                    Ok(work) => work,
                    Err(mpsc::RecvTimeoutError::Timeout) => Work::Read,
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                };
                let paths = match &paths {
                    Ok(paths) => paths,
                    Err(why) => {
                        let moved = matches!(work, Work::At(_));
                        let read = Err(why.clone());
                        if reply_tx.send(Reply::Read { read, moved }).is_err() {
                            return;
                        }
                        wake();
                        continue;
                    }
                };
                let mut at = point;
                let moved = matches!(work, Work::At(_));
                match work {
                    Work::Write(staged) => {
                        let wrote = crate::tables_store::write(paths, staged);
                        if reply_tx.send(Reply::Wrote(wrote)).is_err() {
                            return;
                        }
                    }
                    Work::At(asked) => at = asked,
                    Work::Read => {}
                }
                let read = crate::tables_store::read(paths, at);
                if read.is_ok() {
                    point = at;
                }
                if reply_tx.send(Reply::Read { read, moved }).is_err() {
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
            asked: 0,
            reads: 0,
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
                matches!(reply, Reply::Read { .. }),
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
        self.take(Reply::Read {
            read: Ok(snapshot),
            moved: false,
        });
    }

    fn take(&mut self, reply: Reply) {
        match reply {
            Reply::Read {
                read: Ok(snapshot),
                moved,
            } => {
                self.snapshot = Some(snapshot);
                self.unread = None;
                self.reads = self.reads.wrapping_add(1);
                if moved {
                    self.asked = self.asked.saturating_sub(1);
                }
            }
            Reply::Read {
                read: Err(why),
                moved,
            } => {
                if moved {
                    self.asked = self.asked.saturating_sub(1);
                }
                if self.snapshot.is_none() {
                    self.unread = Some(why);
                } else if moved {
                    self.note = Some(format!("Not read: {why}"));
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

    /// The past point the page shows, where it shows one.
    fn as_of(&self) -> Option<&AsOf> {
        self.snapshot.as_ref().and_then(|s| s.at.as_ref())
    }

    /// Whether the page may move to another point: nothing staged or being written, which a
    /// past table could not take.
    fn movable(&self) -> bool {
        self.staged.is_empty() && !self.writing
    }

    /// Whether the page takes no edit: it shows a past point, or is moving to another one.
    fn fixed(&self) -> bool {
        self.asked > 0 || self.as_of().is_some()
    }

    /// Read the table as of `at` (as it is, where `None`); with cells not written, say why not.
    fn go(&mut self, at: Option<[u8; 32]>) {
        if !self.movable() {
            self.note = Some("Write or discard the changes not written first".to_owned());
            return;
        }
        if self.work.send(Work::At(at)).is_ok() {
            self.asked += 1;
            self.note = None;
        }
    }

    fn sheet(&self, name: &str) -> Option<&Sheet> {
        self.snapshot
            .as_ref()
            .and_then(|s| s.sheets.iter().find(|sheet| sheet.name == name))
    }

    fn staged_in(&self, sheet: &str) -> usize {
        self.staged.keys().filter(|(c, ..)| c == sheet).count()
    }

    /// Stage `text` in a cell, or take back what was staged where it is the written value. A
    /// past table takes no edit.
    fn stage(&mut self, sheet: &str, pk: &str, column: &str, text: String) {
        self.stage_over(sheet, pk, column, text, None);
    }

    /// [`Self::stage`], for a cell the person last saw holding `saw` (an open cell, which covers
    /// what a read brings while it is open): the Write is compared against that, not against
    /// the read the cell is staged on.
    fn stage_over(&mut self, sheet: &str, pk: &str, column: &str, text: String, saw: Option<Seen>) {
        if self.fixed() {
            return;
        }
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
            self.put(at, text, saw);
        }
    }

    /// What the cell at `at` holds in this read, as a Write is compared against it.
    fn held(&self, sheet: &str, pk: &str, column: &str) -> Option<Seen> {
        self.sheet(sheet)
            .and_then(|s| s.base.rows.iter().find(|(row, _)| row == pk))
            .and_then(|(_, row)| row.get(column))
            .cloned()
    }

    /// Stage `text` at `at`, remembering the first time what the person saw the cell hold:
    /// `saw` where the cell was open, else what it holds in this read.
    fn put(&mut self, at: At, text: String, saw: Option<Seen>) {
        if !self.seen.contains_key(&at)
            && let Some(seen) = saw.or_else(|| self.held(&at.0, &at.1, &at.2))
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
        if self.fixed() {
            return false;
        }
        let Some(sheet) = self.sheet(sheet) else {
            return false;
        };
        sheet
            .columns
            .iter()
            .find(|c| c.name == column)
            .is_some_and(|c| c.editable)
            && sheet
                .rows
                .iter()
                .find(|r| r.pk == pk)
                .and_then(|r| r.cells.get(column))
                .is_some_and(|c| !c.raw)
    }

    fn write(&mut self) {
        if self.staged.is_empty() || self.writing || self.fixed() {
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
    /// What the registry held in the cell when it was opened: the open field covers the cell,
    /// so a value another Mac writes while it is open is one the person never saw.
    saw: Option<Seen>,
    input: TextInputState,
}

/// The rows a sheet shows, top down, worked out once for each read, find and sort: a staged
/// cell, a key and a redraw leave it as it is, so a row never moves under the selection.
#[derive(Default)]
struct Order {
    /// What it was worked out from: the sheet, the read ([`TablesApp::reads`]), the find's
    /// words and the sorted column with its direction.
    of: Option<OrderOf>,
    /// Each shown row's place among the sheet's rows.
    rows: Vec<usize>,
    /// Each shown row's key.
    keys: Vec<String>,
    /// Steps each time the rows shown, or their order, differ from the ones before: a range of
    /// cells holds only while it stands ([`Corner::order`]).
    stamp: u64,
    /// How many times an order was worked out.
    built: u64,
}

type OrderOf = (&'static str, u64, String, Option<(String, bool)>);

/// A range's other corner: the cell it began at, among the rows as they stood then.
struct Corner {
    pk: String,
    column: String,
    /// The [`Order::stamp`] the range was grown on. Once the rows shown or their order change,
    /// the rows between two corners are other rows, so the range is the one selected cell again.
    order: u64,
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
    /// How far the grid's last layout could scroll across.
    across_limit: Cell<f32>,
    /// The rows the shown sheet lists, in order, as last worked out.
    order: std::cell::RefCell<Order>,
    /// The widths a person gave columns, by sheet and column: view state, kept across reads.
    widths: BTreeMap<String, BTreeMap<String, f32>>,
    /// How wide each column of the shown sheet drew last.
    drawn: std::cell::RefCell<BTreeMap<String, f32>>,
    /// The column whose header edge is held, and how wide it was when the hold began.
    resizing: Option<(String, f32)>,
    /// The other corner of a range of cells, where Shift and an arrow grew the selection past
    /// its one cell: the row and column it began at. `None` is the one selected cell.
    anchor: Option<Corner>,
    /// The column each sheet's rows are ordered by, and whether from its greatest value down:
    /// view state, kept across reads and written nowhere. A sheet not named shows its rows in
    /// the order the registry holds them.
    sort: BTreeMap<String, (String, bool)>,
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
            across_limit: Cell::new(0.0),
            order: std::cell::RefCell::new(Order::default()),
            widths: BTreeMap::new(),
            drawn: std::cell::RefCell::new(BTreeMap::new()),
            resizing: None,
            anchor: None,
            sort: BTreeMap::new(),
        }
    }
}

const SHEET: &str = "tables/sheet/";
const CELL: &str = "tables/cell/";
const UNDO: &str = "tables/pane/undo/";
const KEEP: &str = "tables/pane/keep/";
const EDIT: &str = "tables/edit";
const FIND: &str = "tables/find";
pub(crate) const WRITE: &str = "tables/write";
const DISCARD: &str = "tables/discard";
const AS_OF: &str = "tables/pane/asof/";
const NOW: &str = "tables/now";
/// A column's right edge in the header line: held and dragged, it sizes the column.
const EDGE: &str = "tables/edge/";
/// A column's header.
const HEAD: &str = "tables/head/";
/// The narrowest and the widest a person can make a column, in points at the text scale of 1.
const NARROWEST: f32 = 36.0;
const WIDEST: f32 = 900.0;
/// How much Left or Right on a column's focused edge sizes it.
const WIDTH_STEP: f32 = 16.0;
/// A paste, which the window sends with the clipboard's text: into the field with focus, or
/// from the selected cell on ([`TablesApp::paste`]).
pub(crate) const PASTE: &str = "tables/paste";

impl TablesViewState {
    /// Scroll an `act` on a cell, a header or its edge outside the grid's room into view; whether
    /// anything moved.
    pub(crate) fn reveal_listed_control(&mut self, key: &UiKey) -> bool {
        if let Some(column) = key
            .as_str()
            .strip_prefix(EDGE)
            .or_else(|| key.as_str().strip_prefix(HEAD))
        {
            let across = self.across;
            self.reveal(column);
            return across != self.across;
        }
        let Some((pk, column)) = key
            .as_str()
            .strip_prefix(CELL)
            .and_then(|rest| rest.split_once('/'))
        else {
            return false;
        };
        let Some(at) = self.order.borrow().keys.iter().position(|key| key == pk) else {
            return false;
        };
        let (scroll, across) = (self.scroll, self.across);
        self.keep_in_view(at);
        let column = column.split('/').next().unwrap_or(column).to_owned();
        self.reveal(&column);
        scroll != self.scroll || across != self.across
    }

    fn focus(&mut self, key: &str) {
        self.common.last_focus = Some(UiKey::new(key.to_owned()));
    }

    /// The rows the find field leaves, a row with a staged cell among them, in the order the
    /// sheet's sorted column gives ([`Self::sort`]); in the sheet's own order where none is
    /// sorted. Only this view is ordered: the sheet and what a Write is compared against are not.
    ///
    /// A cell stands by the value the registry holds, never by words staged in it, and the order
    /// is worked out once for each read, find and sort ([`Order`]): typing into the sorted column
    /// leaves every row where it is, so Return and Tab reach the cell that was beside the edited
    /// one. A staged cell takes its place once it is written and read back.
    fn shown_rows<'a>(&self, app: &TablesApp, sheet: &'a Sheet) -> Vec<&'a SheetRow> {
        self.put_in_order(app, sheet);
        let order = self.order.borrow();
        order
            .rows
            .iter()
            .filter_map(|at| sheet.rows.get(*at))
            .collect()
    }

    /// How many rows the grid shows of `sheet`.
    fn shown_count(&self, app: &TablesApp, sheet: &Sheet) -> usize {
        self.put_in_order(app, sheet);
        self.order.borrow().rows.len()
    }

    /// Work [`Self::order`] out where the read, the find or the sort it was worked out from is
    /// no longer the one in force.
    fn put_in_order(&self, app: &TablesApp, sheet: &Sheet) {
        let find = self.find.value().trim().to_lowercase();
        let of: OrderOf = (
            sheet.name,
            app.reads,
            find,
            self.sort.get(sheet.name).cloned(),
        );
        if self.order.borrow().of.as_ref() == Some(&of) {
            return;
        }
        let find = of.2.as_str();
        let mut rows: Vec<usize> = sheet
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                find.is_empty()
                    || row.pk.starts_with(find)
                    || app
                        .staged
                        .keys()
                        .any(|(c, p, _)| c == sheet.name && *p == row.pk)
                    || sheet.columns.iter().any(|column| {
                        column.group != Group::Linked
                            && row
                                .cells
                                .get(&column.name)
                                .is_some_and(|c| c.text.to_lowercase().contains(find))
                    })
            })
            .map(|(at, _)| at)
            .collect();
        if let Some((column, descending)) = self.sorted(sheet) {
            // Each cell's place is read from its words once, not at every comparison.
            let mut ranked: Vec<(Option<Rank<'_>>, usize)> = rows
                .iter()
                .map(|at| {
                    let shown = sheet.rows[*at]
                        .cells
                        .get(&column.name)
                        .map_or("", |c| c.text.as_str());
                    (
                        (!shown.is_empty()).then(|| Rank::of(column.kind, shown)),
                        *at,
                    )
                })
                .collect();
            // Stable: rows the column holds equal keep the sheet's order. Empty cells come last
            // whichever way the rest run.
            ranked.sort_by(|(a, _), (b, _)| match (a, b) {
                (None, None) => std::cmp::Ordering::Equal,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (Some(_), None) => std::cmp::Ordering::Less,
                (Some(a), Some(b)) => {
                    let order = a.cmp(b);
                    if descending { order.reverse() } else { order }
                }
            });
            rows.clear();
            rows.extend(ranked.into_iter().map(|(_, at)| at));
        }
        let mut order = self.order.borrow_mut();
        let same = order.keys.len() == rows.len()
            && order
                .keys
                .iter()
                .zip(&rows)
                .all(|(key, at)| *key == sheet.rows[*at].pk);
        if !same {
            order.keys = rows.iter().map(|at| sheet.rows[*at].pk.clone()).collect();
            order.stamp = order.stamp.wrapping_add(1);
        }
        order.rows = rows;
        order.of = Some(of);
        order.built = order.built.wrapping_add(1);
    }

    /// The range's other corner, while the rows stand as they did when it was grown. The order
    /// is the one last worked out: a caller reads the rows first ([`Self::shown_rows`]).
    fn corner(&self) -> Option<&Corner> {
        self.anchor
            .as_ref()
            .filter(|corner| corner.order == self.order.borrow().stamp)
    }

    /// The column `sheet`'s rows are ordered by, where one of its columns is sorted, and
    /// whether from its greatest value down.
    fn sorted<'a>(&self, sheet: &'a Sheet) -> Option<(&'a SheetColumn, bool)> {
        let (by, descending) = self.sort.get(sheet.name)?;
        let column = sheet.columns.iter().find(|c| c.name == *by)?;
        Some((column, *descending))
    }

    /// A press on `column`'s header: its rows from the least value up, then from the greatest
    /// down, then in the sheet's own order again. An open cell is staged first, so its field
    /// does not move with its row; the selected row stays in view, and a range whose rows moved
    /// is one cell again ([`Corner::order`]).
    fn sort_by(&mut self, app: &mut TablesApp, column: &str) {
        let head = format!("{HEAD}{column}");
        let keyboard = self.keyboard_on(&head);
        self.close_edit(app, false);
        let next = match self.sort.get(self.sheet) {
            Some((by, false)) if by == column => Some(true),
            Some((by, true)) if by == column => None,
            _ => Some(false),
        };
        match next {
            Some(descending) => {
                self.sort
                    .insert(self.sheet.to_owned(), (column.to_owned(), descending));
            }
            None => {
                self.sort.remove(self.sheet);
            }
        }
        self.rest_focus(&head, keyboard);
        let Some(sheet) = app.sheet(self.sheet) else {
            return;
        };
        let at = self.row.as_ref().and_then(|pk| {
            self.shown_rows(app, sheet)
                .iter()
                .position(|row| row.pk == *pk)
        });
        match at {
            Some(at) => self.keep_in_view(at),
            None => self.scroll = 0.0,
        }
    }

    /// Whether the keyboard's own focus stands on `key`: Tab reached it and its ring shows, so
    /// Return presses it. A pointer's press puts focus on what it pressed without that ring.
    fn keyboard_on(&self, key: &str) -> bool {
        self.common.focus_visible
            && self
                .common
                .last_focus
                .as_ref()
                .is_some_and(|focus| focus.as_str() == key)
    }

    /// Where the keys go after a press on `key`, a control of the sheet that is no cell (a
    /// header, a column's edge). Pressed from the keyboard, they stay on it, so Return presses
    /// it again. After a pointer's press they return to the selected cell, where there is one:
    /// its ring says where the arrows, typing, Delete and a copy go, and it is still drawn.
    fn rest_focus(&mut self, key: &str, keyboard: bool) {
        match (keyboard, &self.row, &self.column) {
            (false, Some(pk), Some(column)) => self.focus(&cell_key(pk, column)),
            _ => self.focus(key),
        }
    }

    /// Scroll so the grid's row `at` shows whole.
    fn keep_in_view(&mut self, at: usize) {
        let line = row_height();
        let top = at as f32 * line;
        let room = self.room.get();
        if top < self.scroll {
            self.scroll = top;
        } else if room > 0.0 && top + line > self.scroll + room {
            self.scroll = top + line - room;
        }
    }

    /// Close the open cell: stage what it holds unless `cancel` or it is what it opened with.
    fn close_edit(&mut self, app: &mut TablesApp, cancel: bool) {
        let Some(editing) = self.editing.take() else {
            return;
        };
        if !cancel && editing.input.value() != editing.seed {
            app.stage_over(
                self.sheet,
                &editing.pk,
                &editing.column,
                editing.input.value().to_owned(),
                editing.saw,
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
        if !col.editable || shown.is_some_and(|c| c.raw) || app.fixed() {
            return;
        }
        let at = (self.sheet.to_owned(), pk.to_owned(), column.to_owned());
        let seed = app.staged.get(&at).cloned().unwrap_or_else(|| {
            if shown.is_some_and(|c| !c.values.is_empty()) {
                String::new()
            } else {
                shown.map(|c| c.text.clone()).unwrap_or_default()
            }
        });
        let mut input = TextInputState::new(seed.clone());
        input.select_all();
        self.anchor = None;
        self.editing = Some(Editing {
            pk: pk.to_owned(),
            column: column.to_owned(),
            seed,
            saw: app.held(self.sheet, pk, column),
            input,
        });
        self.focus(EDIT);
    }

    fn select(&mut self, pk: &str, column: &str) {
        self.anchor = None;
        self.row = Some(pk.to_owned());
        self.column = Some(column.to_owned());
        self.pane_scroll = 0.0;
        self.focus(&cell_key(pk, column));
        self.reveal(column);
    }

    /// Scroll across so `column` shows whole. A column wider than the room beside the key
    /// column cannot: the grid stays where it is while the room shows nothing but that column,
    /// and otherwise shows it from its start.
    fn reveal(&mut self, column: &str) {
        let spans = self.spans.borrow();
        let room = self.grid_room.get();
        if let Some((_, start, end)) = spans.iter().find(|(name, ..)| name == column)
            && room > 0.0
        {
            self.across = across_for(self.across, *start, *end, room);
        }
    }

    /// A hold on `column`'s header edge: `input` is the pointer's travel since the press. The
    /// column follows it; a double press that did not travel, or a press from the keyboard,
    /// fits the column to its header and its widest value in every row. At the pointer's
    /// release the keys return to the selected cell ([`Self::rest_focus`]).
    fn size_column(&mut self, app: &TablesApp, column: &str, input: Option<&SemanticInput>) {
        let Some(sheet) = app.sheet(self.sheet) else {
            return;
        };
        let Some(found) = sheet.columns.iter().find(|c| c.name == column) else {
            return;
        };
        let (low, high) = (NARROWEST * scale(), WIDEST * scale());
        let set = |view: &mut Self, width: f32| {
            view.widths
                .entry(sheet.name.to_owned())
                .or_default()
                .insert(column.to_owned(), width.clamp(low, high));
        };
        match input {
            Some(SemanticInput::Drag { dx, done, presses }) => {
                let from = match &self.resizing {
                    Some((held, from)) if held == column => *from,
                    _ => match self.drawn.borrow().get(column) {
                        Some(drawn) => *drawn,
                        None => return,
                    },
                };
                if *done && *presses >= 2 && dx.abs() < 2.0 {
                    set(self, content_width(found, sheet.rows.iter()));
                } else if self.resizing.is_some() || dx.abs() >= 1.0 {
                    set(self, from + dx);
                }
                self.resizing = (!*done).then(|| (column.to_owned(), from));
                if *done {
                    self.rest_focus(&format!("{EDGE}{column}"), false);
                }
            }
            None => set(self, content_width(found, sheet.rows.iter())),
            Some(_) => {}
        }
    }

    /// Left (`-1`) or Right (`1`) on `column`'s focused edge: the column a step narrower or
    /// wider, within the bounds a drag keeps.
    fn step_width(&mut self, column: &str, by: f32) {
        let Some(drawn) = self.drawn.borrow().get(column).copied() else {
            return;
        };
        let width =
            (drawn + by * WIDTH_STEP * scale()).clamp(NARROWEST * scale(), WIDEST * scale());
        self.widths
            .entry(self.sheet.to_owned())
            .or_default()
            .insert(column.to_owned(), width);
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
        self.keep_in_view(row);
    }

    /// Where the selected cell stands among the rows and columns the grid shows, and how many
    /// of each there are: (row, column, rows, columns).
    fn stands(&self, app: &TablesApp) -> Option<(usize, usize, usize, usize)> {
        let sheet = app.sheet(self.sheet)?;
        let shown = self.shown_rows(app, sheet);
        let columns = ordered(sheet);
        let row = shown
            .iter()
            .position(|r| Some(&r.pk) == self.row.as_ref())?;
        let column = columns
            .iter()
            .position(|c| Some(&c.name) == self.column.as_ref())?;
        Some((row, column, shown.len(), columns.len()))
    }

    /// Tab (`1`) or Shift-Tab (`-1`): the cell after or before the selected one, a row's last
    /// cell before the next row's first.
    fn step_across(&mut self, app: &TablesApp, by: i32) {
        let Some((row, column, rows, columns)) = self.stands(app) else {
            return;
        };
        let to = (row * columns + column) as i64 + i64::from(by);
        if to < 0 || to >= (rows * columns) as i64 {
            return;
        }
        let (to_row, to_column) = (to as usize / columns, to as usize % columns);
        self.step(
            app,
            to_row as i32 - row as i32,
            to_column as i32 - column as i32,
        );
    }

    /// Shift and an arrow: the selected cell moves and the range's other corner stays where the
    /// range began; back on that corner the range is the one cell again.
    fn extend(&mut self, app: &TablesApp, rows: i32, columns: i32) {
        if let Some(sheet) = app.sheet(self.sheet) {
            self.put_in_order(app, sheet);
        }
        let began = match self.corner() {
            Some(corner) => Some((corner.pk.clone(), corner.column.clone())),
            None => self.row.clone().zip(self.column.clone()),
        };
        self.step(app, rows, columns);
        let order = self.order.borrow().stamp;
        self.anchor = began
            .filter(|(pk, column)| {
                (Some(pk), Some(column)) != (self.row.as_ref(), self.column.as_ref())
            })
            .map(|(pk, column)| Corner { pk, column, order });
    }

    /// The rows and columns the selection holds, among those the grid shows: the selected cell's
    /// own, or the range from its other corner while that stands ([`Self::corner`]).
    fn range(
        &self,
        app: &TablesApp,
    ) -> Option<(
        std::ops::RangeInclusive<usize>,
        std::ops::RangeInclusive<usize>,
    )> {
        let (row, column, ..) = self.stands(app)?;
        let sheet = app.sheet(self.sheet)?;
        let corner = self.corner().and_then(|corner| {
            Some((
                self.order
                    .borrow()
                    .keys
                    .iter()
                    .position(|key| *key == corner.pk)?,
                ordered(sheet)
                    .iter()
                    .position(|c| c.name == corner.column)?,
            ))
        });
        let (from_row, from_column) = corner.unwrap_or((row, column));
        Some((
            row.min(from_row)..=row.max(from_row),
            column.min(from_column)..=column.max(from_column),
        ))
    }
}

/// Where the grid stands across so the column from `start` to `end` shows in `room`: scrolled
/// no further than it takes to show the column whole. A column wider than the room is left
/// where it is while it fills the room, and is otherwise shown from its start.
fn across_for(across: f32, start: f32, end: f32, room: f32) -> f32 {
    if end - start >= room {
        if (start..=end - room).contains(&across) {
            across
        } else {
            start
        }
    } else if start < across {
        start
    } else if end > across + room {
        (end - room).max(0.0)
    } else {
        across
    }
}

/// A cell's place in its column's order, read from its words once.
enum Rank<'a> {
    /// A whole number or a time, in a column of them.
    Number(i128),
    /// Words: without regard to case, then as they are written.
    Words(String, &'a str),
}

impl<'a> Rank<'a> {
    fn of(kind: ColumnKind, text: &'a str) -> Self {
        match kind {
            ColumnKind::Number | ColumnKind::Time => text
                .trim()
                .parse::<i128>()
                .map_or_else(|_| Self::Words(text.to_lowercase(), text), Self::Number),
            ColumnKind::Text => Self::Words(text.to_lowercase(), text),
        }
    }

    /// Numbers and times by their value, one that is no number after those that are; words
    /// without regard to case ([`natural_lowered`]), those that differ only in case in their own
    /// order.
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        match (self, other) {
            (Self::Number(a), Self::Number(b)) => a.cmp(b),
            (Self::Number(_), Self::Words(..)) => Ordering::Less,
            (Self::Words(..), Self::Number(_)) => Ordering::Greater,
            (Self::Words(low_a, a), Self::Words(low_b, b)) => {
                natural_lowered(low_a, low_b).then_with(|| a.cmp(b))
            }
        }
    }
}

/// Words already lowered, in order, each run of digits in them by its value: `m9` before
/// `m55`, `rack 2` before `rack 10`; `Equal` where they differ only in the leading zeros of a
/// run. [`Rank::cmp`] then stands words that differ only in case or zeros in their own order.
fn natural_lowered(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    /// A leading run of digits, without its leading zeros, and what follows it.
    fn digits(text: &str) -> (&str, &str) {
        let end = text
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(text.len());
        (text[..end].trim_start_matches('0'), &text[end..])
    }
    let (mut x, mut y) = (a, b);
    loop {
        match (x.chars().next(), y.chars().next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(p), Some(q)) if p.is_ascii_digit() && q.is_ascii_digit() => {
                let ((run_x, rest_x), (run_y, rest_y)) = (digits(x), digits(y));
                let order = run_x.len().cmp(&run_y.len()).then_with(|| run_x.cmp(run_y));
                if order != Ordering::Equal {
                    return order;
                }
                (x, y) = (rest_x, rest_y);
            }
            (Some(p), Some(q)) => {
                if p != q {
                    return p.cmp(&q);
                }
                (x, y) = (&x[p.len_utf8()..], &y[q.len_utf8()..]);
            }
        }
    }
}

/// The cell a pasted field lands on ([`TablesApp::paste`]).
struct Landing {
    pk: String,
    column: String,
    /// Whether the page edits the cell: its column takes an edit and this build reads its value.
    takes: bool,
}

/// A cell's words as one field of tab-separated text. Words that begin with `"` or hold a tab
/// or a line end are set between `"`, each `"` in them doubled, as a spreadsheet writes them:
/// it reads a field that begins with `"` as running to its closing one, tabs and line ends
/// inside it part of the cell.
fn tsv_field(text: &str) -> String {
    if text.starts_with('"') || text.contains(['\t', '\n', '\r']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text.to_owned()
    }
}

/// The cells of tab-separated text, as a spreadsheet writes them and [`tsv_field`] does: a line
/// is a row and a tab parts its cells; a cell that begins with `"` runs to its closing `"`,
/// holding tabs and line ends as they are and `""` for one `"`. A `"` that never closes is the
/// character itself. A line end at the very end ends the last row and begins no other.
fn cells_of(text: &str) -> Vec<Vec<String>> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let mut cell = String::new();
        // A quoted cell: to the `"` that is not one of a pair.
        if let Some(quoted) = rest.strip_prefix('"') {
            let mut at = 0;
            let closed = loop {
                match quoted[at..].find('"') {
                    None => break None,
                    Some(found) if quoted[at + found + 1..].starts_with('"') => {
                        cell.push_str(&quoted[at..=at + found]);
                        at += found + 2;
                    }
                    Some(found) => {
                        cell.push_str(&quoted[at..at + found]);
                        break Some(at + found + 1);
                    }
                }
            };
            match closed {
                Some(end) => rest = &quoted[end..],
                None => cell.clear(),
            }
        }
        // What follows, to the tab or the line end that ends the cell.
        let end = rest.find(['\t', '\n', '\r']).unwrap_or(rest.len());
        cell.push_str(&rest[..end]);
        row.push(cell);
        rest = &rest[end..];
        if let Some(after) = rest.strip_prefix('\t') {
            rest = after;
            if rest.is_empty() {
                row.push(String::new());
            }
            continue;
        }
        let after = rest
            .strip_prefix("\r\n")
            .or_else(|| rest.strip_prefix(['\n', '\r']));
        if let Some(after) = after {
            rest = after;
            rows.push(std::mem::take(&mut row));
        }
    }
    if !row.is_empty() {
        rows.push(row);
    }
    rows
}

fn cell_key(pk: &str, column: &str) -> String {
    format!("{CELL}{pk}/{column}")
}

/// A sheet's columns in the grid's order: its key column, then STORED, SET BY ATERM and LINKED
/// ROWS, each as the schema declares them.
fn ordered(sheet: &Sheet) -> Vec<&SheetColumn> {
    let mut columns: Vec<&SheetColumn> = sheet.columns.iter().collect();
    columns.sort_by_key(|c| (c.name != sheet.key, c.group));
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
                    if (view.row.as_deref(), view.column.as_deref()) != (Some(pk), Some(column)) {
                        view.anchor = None;
                    }
                    view.row = Some(pk.to_owned());
                    view.column = Some(column.to_owned());
                }
                // A cell, a header or an edge the grid holds off its screen comes into view.
                if let Some(key) = focus.as_ref() {
                    view.reveal_listed_control(key);
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
                    if key == PASTE {
                        return self.paste(view, value);
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
                            view.anchor = None;
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
                if let Some(column) = key.strip_prefix(EDGE) {
                    view.size_column(self, column, action.value.as_ref());
                    return true;
                }
                if let Some(column) = key.strip_prefix(HEAD) {
                    let listed = self
                        .sheet(view.sheet)
                        .is_some_and(|sheet| sheet.columns.iter().any(|c| c.name == column));
                    if listed {
                        view.sort_by(self, column);
                    }
                    return listed;
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
                        self.put((view.sheet.to_owned(), pk, column.to_owned()), value, None);
                        self.note = None;
                    }
                    return true;
                }
                if let Some(hex) = key.strip_prefix(AS_OF) {
                    let operation = self.snapshot.as_ref().and_then(|s| {
                        s.sheets
                            .iter()
                            .flat_map(|sheet| &sheet.written)
                            .map(|w| w.operation)
                            .find(|op| aterm_codec::hex::encode_lower(op) == hex)
                    });
                    if let Some(operation) = operation {
                        view.close_edit(self, false);
                        self.go(Some(operation));
                    }
                    return true;
                }
                match key.as_str() {
                    NOW => {
                        self.go(None);
                        true
                    }
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
                        // Enter keeps what was typed and moves to the cell below, as a
                        // spreadsheet's does; Escape leaves the cell as it was.
                        TextInputEvent::Submit => {
                            view.close_edit(self, false);
                            view.step(self, 1, 0);
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
                        TextInputEvent::Left { extend: true } => {
                            view.extend(self, 0, -1);
                            true
                        }
                        TextInputEvent::Right { extend: true } => {
                            view.extend(self, 0, 1);
                            true
                        }
                        TextInputEvent::Left { .. } => {
                            view.step(self, 0, -1);
                            true
                        }
                        TextInputEvent::Right { .. } => {
                            view.step(self, 0, 1);
                            true
                        }
                        // Delete or Backspace empties what the selection holds: staged, as
                        // typing nothing into each cell would be.
                        TextInputEvent::Backspace | TextInputEvent::Delete => {
                            self.clear_selection(view);
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
                        // Escape on a range leaves the one selected cell.
                        TextInputEvent::Cancel if view.anchor.is_some() => {
                            view.anchor = None;
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
                    // Left and Right on a column's focused edge size the column.
                    Some(key) if key.starts_with(EDGE) => {
                        let by = match event {
                            TextInputEvent::Left { .. } => -1.0,
                            TextInputEvent::Right { .. } => 1.0,
                            _ => return false,
                        };
                        view.step_width(&key[EDGE.len()..], by);
                        true
                    }
                    _ => false,
                }
            }
            AppEvent::StepFocus(rows) => {
                view.close_edit(self, false);
                view.step(self, rows, 0);
                true
            }
            AppEvent::StepAcross(by) => {
                view.close_edit(self, false);
                view.step_across(self, by);
                true
            }
            AppEvent::ExtendFocus(rows) => {
                view.close_edit(self, false);
                view.extend(self, rows, 0);
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
            AppEvent::ScrollAcross(lines) => {
                view.across = (view.across + lines as f32 * ACROSS_STEP * scale().max(1.0))
                    .clamp(0.0, view.across_limit.get());
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

    /// Whether Tab (or Shift-Tab, `backwards`) moves within the sheet: focus is on a cell or in
    /// an open one, and a cell lies after (before) it. Past the sheet's last (first) cell Tab
    /// leaves the grid for the next control, as it leaves any other.
    pub(crate) fn tab_stays(&self, view: &TablesViewState, backwards: bool) -> bool {
        let in_sheet = view
            .common
            .last_focus
            .as_ref()
            .is_some_and(|key| key.as_str() == EDIT || key.as_str().starts_with(CELL));
        in_sheet
            && view
                .stands(self)
                .is_some_and(|(row, column, rows, columns)| {
                    if backwards {
                        row > 0 || column > 0
                    } else {
                        row + 1 < rows || column + 1 < columns
                    }
                })
    }

    /// The cells the selection holds, top row first, each row's cells left to right: (row key,
    /// column name) of each.
    fn selection(&self, view: &TablesViewState) -> Vec<Vec<(String, String)>> {
        let (Some(sheet), Some((rows, columns))) = (self.sheet(view.sheet), view.range(self))
        else {
            return Vec::new();
        };
        let shown = view.shown_rows(self, sheet);
        let names = ordered(sheet);
        rows.map(|row| {
            columns
                .clone()
                .map(|column| (shown[row].pk.clone(), names[column].name.clone()))
                .collect()
        })
        .collect()
    }

    /// What a copy takes while focus is on a cell: the words the grid shows in the selected
    /// cell, or in each cell of the range, a row to a line and its cells a tab apart, as a
    /// spreadsheet reads them back ([`tsv_field`]; [`cells_of`] reads them here). A cell shows a
    /// secret's entry name or its mask, never its value, so neither does a copy.
    pub(crate) fn selection_tsv(&self, view: &TablesViewState) -> Option<String> {
        view.common
            .last_focus
            .as_ref()
            .filter(|key| key.as_str().starts_with(CELL))?;
        let sheet = self.sheet(view.sheet)?;
        let lines: Vec<String> = self
            .selection(view)
            .iter()
            .map(|cells| {
                cells
                    .iter()
                    .map(|(pk, column)| {
                        let at = (view.sheet.to_owned(), pk.clone(), column.clone());
                        let text = self.staged.get(&at).cloned().unwrap_or_else(|| {
                            sheet
                                .rows
                                .iter()
                                .find(|row| row.pk == *pk)
                                .and_then(|row| row.cells.get(column))
                                .map(|cell| cell.text.clone())
                                .unwrap_or_default()
                        });
                        tsv_field(&text)
                    })
                    .collect::<Vec<_>>()
                    .join("\t")
            })
            .collect();
        (!lines.is_empty()).then(|| lines.join("\n"))
    }

    /// Stage nothing in each cell of the selection that takes an edit and shows something: a
    /// column the page does not edit, a value this build cannot read and a past table are left.
    fn clear_selection(&mut self, view: &TablesViewState) {
        if self.fixed() {
            return;
        }
        let sheet = view.sheet;
        for (pk, column) in self.selection(view).into_iter().flatten() {
            let takes = self.sheet(sheet).is_some_and(|s| {
                let shown = s
                    .rows
                    .iter()
                    .find(|row| row.pk == pk)
                    .and_then(|row| row.cells.get(&column));
                s.columns.iter().any(|c| c.name == column && c.editable)
                    && !shown.is_some_and(|cell| cell.raw)
            });
            if takes {
                self.stage(sheet, &pk, &column, String::new());
            }
        }
    }

    /// A paste of `text`. In the open cell or the find field it is typed text. On a selected
    /// cell it is a block of cells ([`cells_of`]): its lines go down the rows the grid shows
    /// from the selection's first cell and each line's cells across the columns, every one
    /// staged where the page edits the cell; one cell pasted on a range fills the range. A
    /// column the page does not edit, a value this build cannot read and what lies past the
    /// sheet's last row or column are left, and the bar says how many. Nothing is written.
    fn paste(&mut self, view: &mut TablesViewState, text: &str) -> bool {
        let focus = view
            .common
            .last_focus
            .as_ref()
            .map(|key| key.as_str().to_owned());
        match focus.as_deref() {
            Some(EDIT) => {
                let Some(editing) = view.editing.as_mut() else {
                    return false;
                };
                edit(&mut editing.input, TextInputEvent::Commit(text.to_owned()));
                return true;
            }
            Some(FIND) => {
                edit(&mut view.find, TextInputEvent::Commit(text.to_owned()));
                view.scroll = 0.0;
                return true;
            }
            Some(key) if key.starts_with(CELL) => {}
            _ => return false,
        }
        if self.fixed() {
            return true;
        }
        let block = cells_of(text);
        let (Some(sheet), Some((rows, columns))) = (self.sheet(view.sheet), view.range(self))
        else {
            return true;
        };
        // The cell each field lands on, where the sheet has one there.
        let shown = view.shown_rows(self, sheet);
        let names = ordered(sheet);
        let lands = |row: usize, column: usize| {
            let (row, column) = (shown.get(row)?, names.get(column)?);
            Some(Landing {
                pk: row.pk.clone(),
                column: column.name.clone(),
                takes: column.editable && !row.cells.get(&column.name).is_some_and(|c| c.raw),
            })
        };
        let mut fields: Vec<(Option<Landing>, String)> = Vec::new();
        let (mut last_row, mut last_column) = (*rows.start(), *columns.start());
        if let [line] = block.as_slice()
            && let [one] = line.as_slice()
        {
            for row in rows.clone() {
                for column in columns.clone() {
                    fields.push((lands(row, column), one.clone()));
                }
            }
            (last_row, last_column) = (*rows.end(), *columns.end());
        } else {
            for (down, line) in block.into_iter().enumerate() {
                for (over, text) in line.into_iter().enumerate() {
                    let (row, column) = (rows.start() + down, columns.start() + over);
                    let on = lands(row, column);
                    if on.is_some() {
                        (last_row, last_column) = (last_row.max(row), last_column.max(column));
                    }
                    fields.push((on, text));
                }
            }
        }
        // The pasted block is the selection, from its first cell to its last.
        let first = lands(*rows.start(), *columns.start());
        let last = lands(last_row, last_column);
        let sheet = view.sheet;
        let (mut pasted, mut left) = (0_usize, 0_usize);
        for (on, text) in fields {
            match on {
                Some(on) if on.takes => {
                    self.stage(sheet, &on.pk, &on.column, text);
                    pasted += 1;
                }
                _ => left += 1,
            }
        }
        if let (Some(first), Some(last)) = (first, last) {
            view.select(&first.pk, &first.column);
            if (&first.pk, &first.column) != (&last.pk, &last.column) {
                view.anchor = Some(Corner {
                    pk: last.pk,
                    column: last.column,
                    order: view.order.borrow().stamp,
                });
            }
        }
        if left > 0 {
            self.note = Some(format!(
                "Pasted {pasted} of {} cells: the rest fall past the sheet's last row or \
                 column, or in cells the page does not edit",
                pasted + left
            ));
        }
        true
    }

    /// Whether a Write would send anything: cells are staged, none is being written, and the
    /// page shows the table as it is.
    fn can_write(&self) -> bool {
        !self.staged.is_empty() && !self.writing && !self.fixed()
    }

    /// Whether File ▸ Save and its key press Write: a Write would send something
    /// ([`Self::can_write`]), or would once it has staged what the open cell holds.
    pub(crate) fn can_save(&self, view: &TablesViewState) -> bool {
        let typed = view
            .editing
            .as_ref()
            .is_some_and(|editing| editing.input.value() != editing.seed);
        !self.writing && !self.fixed() && (typed || !self.staged.is_empty())
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
            icon: AppIcon::Tables,
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

    /// Write and Discard, which the palette lists and Save's key presses: the sheet keeps Tab
    /// for its cells, so neither is a long way off for the keyboard.
    fn commands(&self, _: &Self::ViewState, out: &mut Vec<Command>) {
        out.push(Command {
            id: ActionId::new(WRITE),
            title: "Tables: Write Changes".to_string(),
            shortcut: Some(
                if cfg!(target_os = "macos") {
                    "Cmd-S"
                } else {
                    "Ctrl-S"
                }
                .to_string(),
            ),
            enabled: self.can_write(),
        });
        out.push(Command {
            id: ActionId::new(DISCARD),
            title: "Tables: Discard Changes".to_string(),
            shortcut: None,
            enabled: !self.staged.is_empty() && !self.writing,
        });
    }

    fn presentation(&self, _: &Self::ViewState) -> AppPresentation {
        AppPresentation {
            title: TABLES_TITLE.into(),
            icon: AppIcon::Tables,
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

fn field(key: &str, label: &str, input: &TextInputState, view: &TablesViewState) -> UiNode {
    UiNode::new(
        key.to_owned(),
        UiContent::TextField(
            Control::new(
                TextFieldSpec {
                    label: label.into(),
                    placeholder: Some(label.into()),
                    secret: false,
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
            .value(SemanticValue::Text(input.value().into()))
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

/// How far one notch of a sideways wheel scrolls the grid across.
const ACROSS_STEP: f32 = 48.0;
/// The room a header keeps beside its name for its sort mark and its edge.
const HEAD_ROOM: f32 = 16.0;
/// How wide a column's edge is in the header line.
const EDGE_WIDTH: f32 = 7.0;
/// How wide a sorted header's mark is.
const SORT_MARK: f32 = 10.0;
/// How thick the bar is that says where the columns stand across, and the one down the key
/// column's edge while columns lie under it.
const ACROSS_BAR: f32 = 3.0;

/// How wide `column` is where it shows its header and each of `rows`' values whole.
fn content_width<'a>(column: &SheetColumn, rows: impl Iterator<Item = &'a SheetRow>) -> f32 {
    let header = text_width(&column.label, SemanticRole::Text, StyleRef::Note) + HEAD_ROOM;
    let widest = rows
        .filter_map(|row| row.cells.get(&column.name))
        .map(|cell| {
            let tag = if cell.values.is_empty() { 0.0 } else { 70.0 };
            text_width(&cell.text, SemanticRole::Text, StyleRef::Plain) + tag
        })
        .fold(0.0_f32, f32::max);
    header.max(widest) + 20.0
}

/// How wide each column draws: as a person sized it, else as its header or the widest of its
/// first rows needs, within bounds.
fn widths(view: &TablesViewState, sheet: &Sheet, columns: &[&SheetColumn]) -> Vec<f32> {
    let sized = view.widths.get(sheet.name);
    columns
        .iter()
        .map(|column| {
            sized
                .and_then(|sized| sized.get(&column.name))
                .copied()
                .unwrap_or_else(|| {
                    content_width(column, sheet.rows.iter().take(200))
                        .clamp(56.0 * scale(), 260.0 * scale())
                })
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
            text_width(&staged_words, SemanticRole::Text, StyleRef::Unwritten) + 4.0
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
                StyleRef::Unwritten,
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

/// A header line's cell `width` wide: raised ground, ruled right and bottom.
fn head_cell(key: String, width: f32, children: Vec<UiNode>) -> UiNode {
    styled(key, StyleRef::CellHead)
        .layout(Layout::default().width(Length::Fixed(width)))
        .children(children)
}

/// A column's right edge in the header line ([`EDGE`]): the last points of its header cell. Its
/// name says what a press does (it fits the column); a pointer drags it, and Left and Right
/// size the column from the keyboard.
fn edge(column: &SheetColumn) -> UiNode {
    let key = format!("{EDGE}{}", column.name);
    let handle = UiNode::new(
        key.clone(),
        UiContent::Button(
            Control::new(
                ButtonSpec::new(format!(
                    "Fit {} column to its widest value",
                    field_words(column)
                ))
                .visual_label(String::new())
                .description("Left and Right make the column narrower and wider"),
                ActionId::new(key.clone()),
            )
            .style(StyleRef::Navigation)
            .drags(),
        ),
    )
    .layout(Layout::default().width(Length::Fixed(EDGE_WIDTH)));
    group(format!("{key}/lane"))
        .layout(Layout::row())
        .children(vec![
            group(format!("{key}/lead")).layout(Layout::default().width(Length::Fill)),
            handle,
        ])
}

/// The grid: a sheet of ruled cells. The group line and the header line stay at its top, the
/// row numbers and the key column at its left; every row the find leaves is listed under them.
#[expect(clippy::too_many_lines, reason = "one grid, laid out in one pass")]
fn grid(app: &TablesApp, view: &TablesViewState, sheet: &Sheet, width: f32, height: f32) -> UiNode {
    let columns = ordered(sheet);
    let mut widths = widths(view, sheet, &columns);
    let line = row_height();
    let head_line = 22.0 * scale().max(1.0);
    let rows = view.shown_rows(app, sheet);
    // The frame rules the grid's four sides: a point at each.
    let room_across = (width - 2.0).max(1.0);
    // The row numbers stay at the left, then the key column (the first), at most half of what
    // is left; the rest scroll across in the room beside them.
    let gutter = (text_width(
        &rows.len().max(1).to_string(),
        SemanticRole::Text,
        StyleRef::Primary,
    ) + 16.0)
        .max(36.0 * scale())
        .min(room_across * 0.25);
    let after_gutter = (room_across - gutter).max(1.0);
    if let Some(key) = widths.first_mut() {
        *key = key.min(after_gutter * 0.5);
    }
    let pinned = widths.first().copied().unwrap_or(0.0);
    let room_beside = (after_gutter - pinned).max(1.0);
    view.grid_room.set(room_beside);
    let beside: f32 = widths.iter().skip(1).sum();
    {
        let mut spans = view.spans.borrow_mut();
        spans.clear();
        let mut at = 0.0;
        for (column, w) in columns.iter().zip(&widths).skip(1) {
            spans.push((column.name.clone(), at, at + w));
            at += w;
        }
        *view.drawn.borrow_mut() = columns
            .iter()
            .zip(&widths)
            .map(|(column, w)| (column.name.clone(), *w))
            .collect();
    }
    let across_limit = (beside - room_beside).max(0.0);
    view.across_limit.set(across_limit);
    let across = view.across.min(across_limit);
    // A line of the grid: its number's and its key column's seats, then the rest, scrolled
    // across and clipped where the key column ends.
    // `listed` keeps what the lane scrolls off its side in the tree, as the rows' list keeps
    // theirs: a header out of view is still named, and comes into view when focus reaches it.
    let lane = |key: String, mut children: Vec<UiNode>, h: f32, listed: bool| {
        let rest = if children.len() > 2 {
            children.split_off(2)
        } else {
            Vec::new()
        };
        let mut beside = group(format!("{key}/across"))
            .layout(
                Layout::row()
                    .height(Length::Fixed(h))
                    .clipped()
                    .scrolled(across),
            )
            .children(rest);
        if listed {
            beside = beside.room(h, false).lists_every_row();
        }
        children.push(beside);
        group(key)
            .layout(Layout::row().height(Length::Fixed(h)))
            .children(children)
    };
    // The group line: each group's word over its first column, its span ruled off. The key
    // column carries its group's word, so a group it begins reads once, over it.
    let mut groups = vec![head_cell("tables/groups/#".into(), gutter, Vec::new())];
    if let Some(first) = columns.first() {
        groups.push(head_cell(
            "tables/groups/0".into(),
            pinned,
            vec![padded(
                "tables/groups/0/pad".into(),
                fitted(
                    "tables/groups/0/word",
                    first.group.word(),
                    StyleRef::Note,
                    pinned - 10.0,
                ),
                pinned,
            )],
        ));
    }
    let mut at = 1;
    while at < columns.len() {
        let group_of = columns[at].group;
        let mut end = at;
        let mut span = 0.0;
        while end < columns.len() && columns[end].group == group_of {
            span += widths[end];
            end += 1;
        }
        let word = if at == 1 && group_of == columns[0].group {
            ""
        } else if group_of == Group::Linked && span < 96.0 {
            "LINKED"
        } else {
            group_of.word()
        };
        groups.push(head_cell(
            format!("tables/groups/{at}"),
            span,
            vec![padded(
                format!("tables/groups/{at}/pad"),
                fitted(
                    format!("tables/groups/{at}/word"),
                    word,
                    StyleRef::Note,
                    span - 10.0,
                ),
                span,
            )],
        ));
        at = end;
    }
    // The header line: `#` over the row numbers, then each column's name and its edge.
    let mut headers = vec![head_cell(
        "tables/head/#/seat".into(),
        gutter,
        vec![padded(
            "tables/head/#/pad".into(),
            fitted("tables/head/#", "#", StyleRef::Note, gutter - 10.0),
            gutter,
        )],
    )];
    let sorted = view.sorted(sheet);
    for (column, w) in columns.iter().zip(&widths) {
        // A header is a press that sorts by its column: its name says so, in the words a
        // cell's is said in, and how the column is sorted; its face wears a drawn mark.
        let key = format!("{HEAD}{}", column.name);
        let state = sorted
            .filter(|(by, _)| by.name == column.name)
            .map(|(_, descending)| descending);
        let (mark, said) = match state {
            Some(false) => (Some(StyleRef::SortUp), ", sorted ascending"),
            Some(true) => (Some(StyleRef::SortDown), ", sorted descending"),
            None => (None, ""),
        };
        let mark_w = if mark.is_some() { SORT_MARK } else { 0.0 };
        let mut face = vec![fitted(
            format!("{key}/word"),
            &column.label,
            StyleRef::Note,
            (w - 20.0 - mark_w).max(1.0),
        )];
        if let Some(mark) = mark {
            face.push(group(format!("{key}/lead")).layout(Layout::default().width(Length::Fill)));
            face.push(
                styled(format!("{key}/mark"), mark)
                    .layout(Layout::default().width(Length::Fixed(mark_w))),
            );
        }
        headers.push(head_cell(
            format!("{key}/seat"),
            *w,
            vec![
                press(&key, format!("Sort by {}{said}", field_words(column))),
                group(format!("{key}/face"))
                    .layout(Layout::row().padding(Insets::symmetric(10.0, 0.0)))
                    .children(face)
                    .paint_only_tree(),
                edge(column),
            ],
        ));
    }
    let room = (height - 3.0 - head_line - line).max(0.0);
    let limit = (rows.len() as f32 * line - room).max(0.0);
    view.room.set(room);
    view.limit.set(limit);
    let scroll = view.scroll.min(limit);
    // A cell far from what the grid shows keeps its seat and its press, which name it to the
    // keyboard and to assistive technology, and draws nothing.
    let (over_rows, over_across) = (line * 6.0, 240.0);
    let range = view.corner().and_then(|_| view.range(app));
    let mut lines = Vec::with_capacity(rows.len());
    for (n, row) in rows.iter().enumerate() {
        let top = n as f32 * line;
        let near_row = top + line >= scroll - over_rows && top <= scroll + room + over_rows;
        let selected = view.row.as_deref() == Some(row.pk.as_str());
        let mut cells = Vec::with_capacity(columns.len() + 1);
        let number_key = format!("tables/num/{}", row.pk);
        let number = if near_row {
            // On the header ground, where the note's ink reads and the tertiary one does not.
            let style = if selected {
                StyleRef::Primary
            } else {
                StyleRef::Note
            };
            let words = (n + 1).to_string();
            let words_w = text_width(&words, SemanticRole::Text, style).min(gutter - 12.0);
            vec![
                group(format!("{number_key}/row"))
                    .layout(Layout::row().padding(Insets {
                        right: 8.0,
                        ..Insets::default()
                    }))
                    .children(vec![
                        group(format!("{number_key}/lead"))
                            .layout(Layout::default().width(Length::Fill)),
                        fitted(format!("{number_key}/n"), &words, style, words_w),
                    ]),
            ]
        } else {
            Vec::new()
        };
        cells.push(head_cell(number_key, gutter, number).layout_height(line));
        let mut left = 0.0;
        for (at, (column, w)) in columns.iter().zip(&widths).enumerate() {
            let near = near_row
                && (at == 0
                    || (left + w >= across - over_across
                        && left <= across + room_beside + over_across));
            if at > 0 {
                left += w;
            }
            // A cell a range holds beside the selected one.
            let ranged = range
                .as_ref()
                .is_some_and(|(rows, columns)| rows.contains(&n) && columns.contains(&at));
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
                    field(EDIT, &column.label, &editing.input, view).layout(
                        Layout::default()
                            .width(Length::Fixed(*w))
                            .height(Length::Fixed(line)),
                    ),
                );
            } else {
                if staged.is_some() && near {
                    seat.push(styled(format!("{key}/staged"), StyleRef::Unwritten));
                }
                let text = staged
                    .cloned()
                    .unwrap_or_else(|| shown.map(|c| c.text.clone()).unwrap_or_default());
                let conflict = staged.is_none() && shown.is_some_and(|c| !c.values.is_empty());
                let changed = app.as_of().is_some()
                    && sheet
                        .since
                        .contains_key(&(row.pk.clone(), column.name.clone()));
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
                if changed {
                    spoken.push_str(", changed since");
                }
                if ranged {
                    spoken.push_str(", selected");
                    // Under the ring on the selected cell and the blue of a staged one.
                    if near && !focused && staged.is_none() {
                        seat.push(styled(format!("{key}/range"), StyleRef::CellRange).paint_only());
                    }
                }
                seat.push(press(&key, spoken));
                let style = if column.group == Group::Stored {
                    StyleRef::Plain
                } else {
                    StyleRef::Secondary
                };
                if near {
                    let mut face = Vec::new();
                    if conflict || changed {
                        let n = shown.map_or(0, |c| c.values.len());
                        let tag = if conflict {
                            format!("{n} values")
                        } else {
                            "changed".to_owned()
                        };
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
                    } else if column.kind == ColumnKind::Number {
                        let tw = text_width(&text, SemanticRole::Text, style).min(w - 20.0);
                        face.push(
                            group(format!("{key}/lead"))
                                .layout(Layout::default().width(Length::Fill)),
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
                if focused && near {
                    // The selection's ring, over the cell: neutral ink, whatever the cell holds.
                    seat.push(styled(format!("{key}/focus"), StyleRef::CellSelected).paint_only());
                }
            }
            cells.push(
                styled(format!("{key}/seat"), StyleRef::Cell)
                    .layout(
                        Layout::default()
                            .width(Length::Fixed(*w))
                            .height(Length::Fixed(line)),
                    )
                    .children(seat),
            );
        }
        lines.push(lane(format!("tables/row/{}", row.pk), cells, line, false));
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
                .scrolled(scroll),
        )
        .room(room, limit > 0.0)
        .lists_every_row()
        .children(lines);
    let sheet_lines = group("tables/grid/sheet")
        .layout(Layout::column())
        .children(vec![
            styled("tables/grid/top", StyleRef::Cell)
                .layout(Layout::default().height(Length::Fixed(1.0))),
            lane("tables/groups".into(), groups, head_line, false),
            lane("tables/head".into(), headers, line, true),
            body,
        ]);
    // Over the sheet, where columns lie off its side: a thumb along its foot that says how
    // many and where they stand, and a bar down the key column's edge while some lie under it.
    let mut over = vec![sheet_lines];
    if across_limit > 0.0 {
        let thumb = (room_beside * room_beside / beside)
            .max(24.0)
            .min(room_beside);
        let lead = (room_beside - thumb) * (across / across_limit);
        let edge = if across > 0.0 {
            styled("tables/grid/across/edge", StyleRef::SheetBar)
        } else {
            group("tables/grid/across/edge")
        };
        over.push(
            group("tables/grid/across")
                .layout(Layout::row())
                .children(vec![
                    group("tables/grid/across/frozen").layout(
                        Layout::default()
                            .width(Length::Fixed((gutter + pinned - ACROSS_BAR + 1.0).max(0.0))),
                    ),
                    edge.layout(Layout::default().width(Length::Fixed(ACROSS_BAR - 1.0))),
                    group("tables/grid/across/track")
                        .layout(Layout::column().width(Length::Fixed(room_beside)))
                        .children(vec![
                            group("tables/grid/across/space")
                                .layout(Layout::default().height(Length::Fill)),
                            group("tables/grid/across/foot")
                                .layout(Layout::row().height(Length::Fixed(ACROSS_BAR)))
                                .children(vec![
                                    group("tables/grid/across/lead")
                                        .layout(Layout::default().width(Length::Fixed(lead))),
                                    styled("tables/grid/across/thumb", StyleRef::SheetBar)
                                        .layout(Layout::default().width(Length::Fixed(thumb))),
                                ]),
                        ]),
                ])
                .paint_only_tree(),
        );
    }
    // The frame: a rule down the left and along the top; the cells rule the rest.
    styled("tables/grid", StyleRef::Cell)
        .layout(
            Layout::row()
                .padding(Insets {
                    right: 1.0,
                    bottom: 1.0,
                    ..Insets::default()
                })
                .clipped(),
        )
        .children(vec![
            styled("tables/grid/left", StyleRef::Cell)
                .layout(Layout::default().width(Length::Fixed(1.0))),
            group("tables/grid/over").children(over),
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
        StyleRef::Unwritten,
        !app.writing,
    );
    let buttons_w = crate::native_ui::native_button_label_width("Discard")
        + crate::native_ui::native_button_label_width("Writing\u{2026}")
        + 48.0
        + 8.0;
    let count_w = text_width(&count, SemanticRole::Text, StyleRef::Unwritten);
    let note_w = (width - 28.0 - count_w - buttons_w - 24.0).max(0.0);
    let mut children = vec![fitted(
        "tables/bar/count",
        &count,
        StyleRef::Unwritten,
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
        styled("tables/bar", StyleRef::Unwritten)
            .layout(
                Layout::row()
                    .gap(12.0)
                    .height(Length::Fixed(h))
                    .padding(Insets::symmetric(14.0, 0.0)),
            )
            .children(children),
    )
}

/// A writer and when it wrote, in `width`: `{key}/who`, its Mac's label set as `style` (its short
/// id dimmed where no label is known), and `{key}/when`, `· when`.
fn writer(key: &str, by: &Author, when: &str, style: StyleRef, width: f32) -> [UiNode; 2] {
    let who = by.who();
    let when = format!("\u{b7} {when}");
    let who_w = text_width(&who, SemanticRole::Text, by.style(style)).min(width * 0.5);
    [
        fitted(format!("{key}/who"), &who, by.style(style), who_w),
        fitted(
            format!("{key}/when"),
            &when,
            style,
            (width - who_w - 8.0).max(1.0),
        ),
    ]
}

/// The pane's word for a value: `empty`, or the value as the grid shows it.
fn value_words(text: &str) -> String {
    if text.is_empty() {
        "empty".into()
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
            // Keep stages a value, so it is offered only where the page writes the cell.
            let keep = app.keepable(sheet.name, &row.pk, &column.name).then(|| {
                button(
                    &format!("{KEEP}{}/{n}", column.name),
                    "Keep",
                    &format!("Keep {} for {}", value_words(value), field_words(column)),
                    StyleRef::Secondary,
                    true,
                )
            });
            let keep_w = crate::native_ui::native_button_label_width("Keep") + 24.0;
            let half = (inner - 36.0 - keep_w) * 0.5;
            let mut parts = vec![fitted(
                format!("{key}/{n}/value"),
                &value_words(value),
                StyleRef::Plain,
                half,
            )];
            if let Some(by) = by {
                parts.extend(writer(
                    &format!("{key}/{n}"),
                    by,
                    &when_words(by.at_ms, now),
                    StyleRef::Note,
                    half,
                ));
            }
            boxed.push(
                group(format!("{key}/{n}"))
                    .layout(Layout::row().gap(8.0).height(Length::Fixed(line + 4.0)))
                    .children(parts.into_iter().chain(keep).collect()),
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
                StyleRef::Unwritten
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
                    &value_words(&text),
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
                    by.style(StyleRef::Note),
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
                        &format!("Written elsewhere since: {}", value_words(&now)),
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
    // In a past table: each cell of this row changed since, what it holds now and who wrote it.
    if app.as_of().is_some() {
        let changed: Vec<(&&SheetColumn, &Since)> = columns
            .iter()
            .filter_map(|column| {
                sheet
                    .since
                    .get(&(row.pk.clone(), column.name.clone()))
                    .map(|since| (column, since))
            })
            .collect();
        if !changed.is_empty() {
            push(
                &mut children,
                fitted("tables/pane/now", "CHANGED SINCE", StyleRef::Note, inner),
                line + 6.0,
            );
        }
        for (column, since) in changed {
            let key = format!("tables/pane/now/{}", column.name);
            let then = row.cells.get(&column.name).map_or("", |c| c.text.as_str());
            let words = if since.members {
                format!("{}: {}", field_words(column), since.now)
            } else {
                format!(
                    "{}: {} \u{2192} {}",
                    field_words(column),
                    value_words(then),
                    value_words(&since.now)
                )
            };
            let who = since
                .by
                .iter()
                .map(Author::who)
                .collect::<Vec<_>>()
                .join(" / ");
            // One writer is set as its label is known; several in the note's own style.
            let who_style = match since.by.as_slice() {
                [one] => one.style(StyleRef::Note),
                _ => StyleRef::Note,
            };
            let who_w = if since.by.is_empty() {
                0.0
            } else {
                text_width(&who, SemanticRole::Text, who_style).min(meta_w * 1.5)
            };
            let mut parts = vec![fitted(
                format!("{key}/what"),
                &words,
                StyleRef::Plain,
                (inner - who_w - 8.0).max(1.0),
            )];
            if !since.by.is_empty() {
                parts.push(fitted(format!("{key}/who"), &who, who_style, who_w));
            }
            push(
                &mut children,
                group(key).layout(Layout::row().gap(8.0)).children(parts),
                line + 2.0,
            );
        }
    }
    // History: newest first, one entry per operation that set a cell of this row, each offering
    // the table as that operation's causal past holds it.
    let history = row_history(sheet, &row.pk);
    if !history.is_empty() {
        push(
            &mut children,
            fitted("tables/pane/history", "HISTORY", StyleRef::Note, inner),
            line + 6.0,
        );
    }
    let shown_at = app.as_of().map(|at| at.operation);
    for (n, (operation, by, changes)) in history.iter().enumerate().take(60) {
        let key = format!("tables/pane/written/{n}");
        let here = shown_at == Some(*operation);
        let style = if here {
            StyleRef::Primary
        } else {
            StyleRef::Plain
        };
        push(
            &mut children,
            group(format!("{key}/line"))
                .layout(Layout::row().gap(6.0))
                .children(writer(&key, by, &by.when(now), style, inner).into()),
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
        // The table as this operation's causal past holds it.
        push(
            &mut children,
            group(format!("{key}/asof"))
                .layout(Layout::row().padding(Insets {
                    left: 12.0,
                    ..Insets::default()
                }))
                .children(vec![button(
                    &format!("{AS_OF}{}", aterm_codec::hex::encode_lower(operation)),
                    "As of",
                    &format!("The table as of {} \u{b7} {}", by.who(), by.when(now)),
                    StyleRef::Secondary,
                    !here && app.movable(),
                )]),
            line + 2.0,
        );
    }
    pane_frame(children, width, height, view, tall)
}

/// A row's history, newest first: the operation, who, and each field it set as
/// `Field: was → now`.
fn row_history(sheet: &Sheet, pk: &str) -> Vec<([u8; 32], Author, Vec<String>)> {
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
            let line = if cell.now.starts_with("+ ") || cell.now.starts_with('\u{2212}') {
                format!("{}: {}", field_words(column), cell.now)
            } else {
                format!(
                    "{}: {} \u{2192} {}",
                    field_words(column),
                    value_words(was),
                    value_words(&cell.now)
                )
            };
            lines.push(line);
            last.insert(cell.column.as_str(), cell.now.as_str());
        }
        if !lines.is_empty() {
            out.push((written.operation, written.by.clone(), lines));
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
                    rows_words(view.shown_count(app, s), s.rows.len())
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
    let find_w = 220.0 * scale().max(1.0);
    let mut tools = vec![
        field(FIND, "Find", &view.find, view)
            .layout(Layout::default().width(Length::Fixed(find_w))),
    ];
    // A past table: which point, how much changed since, and the way back to now.
    if let Some(at) = app.as_of() {
        let now_ms = aterm_time::unix_millis().unwrap_or(0);
        // The cells the past grid shows and marks: a row born since has no line there.
        let changed = sheet.map_or(0, |s| {
            s.rows
                .iter()
                .flat_map(|row| {
                    s.columns
                        .iter()
                        .map(move |c| (row.pk.clone(), c.name.clone()))
                })
                .filter(|at| s.since.contains_key(at))
                .count()
        });
        let mut words = match &at.by {
            Some(by) => format!("As of {} \u{b7} {}", by.who(), by.placed(now_ms)),
            None => "As of an earlier point".to_owned(),
        };
        words.push_str(&match changed {
            0 => " \u{b7} nothing changed since".to_owned(),
            1 => " \u{b7} 1 cell changed since".to_owned(),
            n => format!(" \u{b7} {n} cells changed since"),
        });
        let now_w = crate::native_ui::native_button_label_width("Now") + 24.0;
        tools.push(fitted(
            "tables/asof",
            &words,
            StyleRef::Plain,
            (main_w - find_w - now_w - 24.0).max(1.0),
        ));
        tools.push(
            group("tables/now-seat")
                .layout(
                    Layout::row()
                        .width(Length::Fixed(now_w))
                        .padding(Insets::symmetric(0.0, 3.0)),
                )
                .children(vec![button(
                    NOW,
                    "Now",
                    "Show the table as it is now",
                    StyleRef::Secondary,
                    true,
                )]),
        );
    }
    main.push(
        group("tables/tools")
            .layout(Layout::row().gap(12.0).height(Length::Fixed(line + 4.0)))
            .children(tools),
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
        // `main_w` is the room inside the column's padding.
        group("tables/main")
            .layout(
                Layout::column()
                    .gap(10.0)
                    .width(Length::Fixed(main_w + 32.0))
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
