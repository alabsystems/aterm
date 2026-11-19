// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Tables page (docs/PLAN.md LT-08): the registry's stored tables, each a grid of its live
//! rows, the owner's edits staged and written in one Write through the same save
//! `aterm db table apply` makes ([`crate::tables_store`]), a row's fields with their writers in
//! the pane beside the grid, and every cell edit of the table in the history drawer under it
//! ([`history`]).
//!
//! - **Rail.** Every collection of the registry generation in use, as its compiled table lists
//!   them ([`crate::tables_store::listed`]): under STORED the owner's, under SET BY ATERM what
//!   each Mac states and the importer's runs, under REFERENCE the compiled reference rows. One
//!   line per table: its name and its rows; the cells of it not yet written, where there are
//!   any. The list scrolls where the window is too short for it, the chosen table kept in view,
//!   and when it was read stays at its foot. A table of each Mac's own rows (an owned
//!   collection) is read only: it sorts, sizes and copies like any sheet, shows its times as
//!   dates, and none of its cells opens, empties or stages. Under the tables a Check line (no
//!   count on it) opens the Check view ([`crate::native_tables_check`]): what
//!   `aterm db alab check` finds, each finding in words with the way to the rows it names, and
//!   the store's state.
//! - **Narrow.** Under [`WIDE`] (900 px) the page stands in one column: the rail is one table
//!   picker ([`PICKER`]) in the title's place, which lists the tables in the grid's place, and
//!   the selected row's pane (Check's selected finding) sits below the grid (its list),
//!   scrolling on its own.
//! - **Where a value came from.** A line under the grid says it of the selected cell
//!   ([`provenance`]): what a Mac states, when it saw it and which Mac (SEEN); what was written
//!   for the owner, at which Mac and when (SET), or that aterm wrote it by itself (AUTOMATIC)
//!   or an import did; and, whatever else it says, what a Mac reports of the cell. Where a Mac's
//!   newest report disagrees with what the cell holds, the line says both values (DIFFERS) and
//!   the grid marks the cell `differs`; neither is written over the other. In the grid a value a
//!   person set reads in the primary ink, one aterm or an import set in the plain one.
//! - **Cell bar.** One line over the grid of every table the rail lists, a row high, at every
//!   width (never over Check or the narrow page's list of tables, which are no table's grid).
//!   The grid gives the bar that row of its own room, so nothing else on the page moves for
//!   it; in a window so short that the grid under it would not keep its name, type and group
//!   lines and one whole row beside the history drawer's line (a narrow page's grid as it
//!   stands with a row selected), the bar gives way and the grid keeps the room. Whether it
//!   stands is the window's to say, with the filters' chips while any stands (counted as the tools
//!   row is, so a filter added in a short window can take the bar away), judged as if changes
//!   waited to be written and the line under the grid stood three lines high: no selection and no
//!   staged change takes the bar away or brings it back, and a line under the grid that wraps
//!   further takes its room from the grid's, down to the three lines the grid keeps without the
//!   bar, so the bar never pushes the page past the window. It says
//!   which cell is selected (its row's key and its column; what of the row's own key tells it
//!   apart, dimmed, where another row shows that key too; a range's rows and columns), the
//!   cell's whole value as the grid shows it and nothing the grid does not (a secret's cell
//!   reads its entry name or its mask there too; a day reads as its date) and the cell's
//!   state where it has one. What a column holds is not said here: the line under its name
//!   says it of every column at once (Types). A state is what the grid marks on the cell: typed
//!   and not written, with what the registry holds (one the check or a Write refuses reads
//!   `refused` first, in the grid's tag's word and ink: its rule is the pane's to say); how
//!   many values a conflict has; that a Mac reports another value, in the words the grid's
//!   cell speaks; a value a newer aterm wrote; in a past table what the cell holds now and who
//!   wrote that. A linked count's value is its count; how many rows of which table name the
//!   row is the line under the grid's to say. Of
//!   the value the cell holds the bar names no writer and no time: where that value came
//!   from is said once, by the line under the grid. A range says how many of its cells are
//!   filled, and of whole numbers their sum, least and most. The cell whose value is said is
//!   one the grid shows: a selected cell whose row the grid leaves out is named by its address,
//!   as the line under the grid still tells of it, with what leaves it out: not among the rows
//!   found (the find), left out by the filters (the find passes it), or not among the live rows
//!   (or the retired ones: the lifecycle filter). Where the line is too narrow the address and
//!   the value stay and the state's words are cut at their end (left out where the value leaves
//!   them no room); only then is the address cut, in its row's key before its column's words,
//!   and the value at the bar's end. Each seat the line shows is spoken whole whatever it cuts. The bar takes no key and
//!   no focus: an open cell's field or list of words keeps them.
//! - **Grid.** A sheet: ruled cells of one height under a header line of column names, each row
//!   numbered at its left. The header line stays at the top while the rows scroll under it, and
//!   the row numbers and the row's key column (`label`, an address's `addr`, a reference row's
//!   own key) stay at the left
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
//!   its place when it is written and read back. On the owner's tables columns sit under STORED
//!   (the owner's), SET WHEN ADDED (set when a row is born) and SET BY ATERM (cells aterm keeps);
//!   on every other table under the word its rail heading says; and LINKED ROWS (how many rows
//!   of another table name this one). A staged cell a new read can no longer write is dropped,
//!   and the page says so. A stored cell is edited in place: a press selects it, a
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
//!   File ▸ Save and the palette's Write press Write; only a person's own press does
//!   (`app_control::PERSON_ONLY_PREFIXES`): `act` and a control or accessibility client are
//!   refused it. Right past the last column goes on to the pane's first Keep or Undo, Up and
//!   Down move among them, Return presses one, and Left (or Up from the first) returns to the
//!   cell. After a pointer's press on a header or a drag of an edge the keys are the selected
//!   cell's again; pressed from the keyboard, a header keeps them. A read that moves the
//!   selected row brings it back into view.
//! - **Types.** Under each column's name a line says what its cells hold, marks a column the
//!   row's key comes from, and names the table a reference leads to; the columns stand in the
//!   display order compiled beside each collection (`aterm_db::typed::Faced::order`), the
//!   column that names a row first. An enum's cell opens on a list of its words, a set of
//!   enums' on a multi-choice, a bool's switches, and a day reads and is typed as its date
//!   ([`crate::native_tables_choices`]). Each staged cell is checked at once by the save's own
//!   reading of the grid ([`crate::tables_store::refusals`]): one it would refuse says
//!   `refused` on its cell, with the rule in the pane, as a cell the Write was refused at does;
//!   the Write bar counts them, and a Write holds back each table that has one, writing the
//!   rest.
//! - **Columns.** The tools row's Columns button (`Columns 15 of 21`) opens a list of the
//!   table's columns under their groups, each with a mark where the grid draws it and how many
//!   of the rows the lifecycle filter shows of the whole table fill it; a press hides or shows
//!   one. The columns no row fills fold into one stub at the end of their group (`5 EMPTY`),
//!   which a press unfolds; a switch in the list turns the fold off. Each header wears its fill
//!   as a bar along its foot. Copy, ranges, Tab's path and the arrows take the drawn columns
//!   alone, and a column with a change not written or a refused cell is never hidden or folded.
//!   The choice is the view's, per table, while the tab lives ([`native_tables_columns`]).
//! - **Filters.** Each column's header carries a mark at its right end (its press of its own;
//!   the header's still sorts) that opens a list of the values its rows hold, each with how many
//!   rows hold it, and the one-press choices `Only rows with a value` and `Only rows that are
//!   empty`; a number's or a time's column also takes `At least` and `At most`. Each filter that
//!   stands is a chip in words in a line under the tools (`ROLE is worker, desk`); filters join
//!   with and, after the lifecycle filter and beside the find, judge what the read holds, and
//!   leave a row with a staged cell and a new row shown. The heading counts the rows shown of
//!   those the lifecycle filter leaves (`24 of 156 rows`). The chips' line takes its room from
//!   the grid's, once, and counts in the window's judgment of the cell bar and the history
//!   drawer as the tools row does. A press that asks for a row they alone leave out (Check's Show
//!   row, a line of the history drawer) clears them; Show row first empties the find and widens
//!   the lifecycle filter where they leave the row out, and a line of the drawer keeps both. The
//!   filters are the view's, per table ([`native_tables_filters`]).
//! - **Blue** means only "not yet written" ([`StyleRef::Unwritten`], the palette's one fixed
//!   [`crate::native_appearance::NOT_WRITTEN`], never the accent): a staged cell, its staged
//!   words in the cell bar and the pane, the rail's count of them, the Write bar, the history
//!   drawer's lines of them. Nothing else on the page wears it.
//! - **Writers** are named by the machine row their Mac's facts bind, as history names it: its
//!   label (`m73`), else its key; a writer no row binds shows the start of its machine id,
//!   dimmed.
//! - **A conflict** shows every value, sorted by its words, and the pane lists each with its
//!   writer and a Keep; no clock picks one. A Write is compared against what each staged cell
//!   held when the person last saw it: when it was staged, or when it was opened, since an
//!   open cell covers what a read brings.
//! - **Secrets** never reach the page: a secret cell names its secret-store entry, read-only here.
//!   Addresses and phone numbers are ordinary data and read as stored.
//! - **History.** A drawer under the grid and the line under it, over the Write bar, closed until
//!   it is asked for, lists the cells each record set, newest first in the registry's order, for
//!   the selected cell, the selected row or the whole table, whatever the find and the filters
//!   show; a line and its grid cell select each other, a line of a column the Columns list hid
//!   shows it again, and one of a row the filters alone leave out clears them (one the find or
//!   the lifecycle filter leaves out is named in its head, the filters kept). It takes its height
//!   out of the grid's room, once, at every width, so nothing else moves for it; every table the
//!   rail lists has one (a table of a Mac's own rows lists what was read of it), and Check and the
//!   narrow page's list of tables have none. Whether it stands is the window's to say, judged on
//!   the room the cell bar is (under the filters' chips while any stands, so a filter added in a
//!   short window can take it away), so no selection and no staged change takes it away or brings
//!   it back; its line comes out of that judged room first, so in a window too short for its line,
//!   the cell bar and the grid under the bar the cell bar gives way first, and the drawer gives
//!   way, with the pane's way to it, only where that room leaves three lines of grid less than its
//!   line, at every width. It never runs the page past the window: a line under the grid wrapped
//!   onto more than three lines takes its room from the grid, down to three lines that the grid,
//!   its bar and the drawer's line keep between them. Its list is one Tab stop, Up and Down moving
//!   among its lines. The pane lists no history: its History of this row opens the drawer on the
//!   row.
//! - **As of.** Each record in the drawer offers the table as that operation's causal past holds
//!   it (`aterm db table get|scan|export --at`, the same projection), read only, each cell changed
//!   since marked in the grid and listed in the pane with what it holds now and who wrote that
//!   (`aterm db table diff <operation> head`); Now returns to the table as it is.
//!
//! The page reads and writes on its own thread; a read answers in a [`Reply`] the host polls on
//! [`crate::Wake::TablesChanged`].

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use aterm_db::typed::grid::{Base, Seen};

use crate::native_app::{
    AppDescriptor, AppEvent, AppIcon, AppIndicators, AppKind, AppPresentation, CloseReadiness,
    CloseRequest, Command, CommonViewState, DamageRegion, EventResult, NativeAppModel,
    SemanticInput, TextInputEvent, UpdateCx, ViewCx,
};
use crate::native_tables_choices::{CHOICE, Choice, Opens};
use crate::native_text_input::TextInputState;
use crate::native_ui::{
    ActionId, ButtonSpec, Control, ControlState, GroupSpec, Insets, Layout, Length, SemanticRole,
    SemanticValue, StyleRef, TextFieldSpec, TextSpec, UiContent, UiKey, UiNode, UiTree,
};
use crate::native_workers::{one_line, text_width};

#[path = "native_tables_claims.rs"]
mod claims;
#[path = "native_tables_history.rs"]
mod history;
pub(crate) use history::registry::{Asked, Opened, Paged};
#[cfg(unix)]
pub(crate) use history::registry::{
    Logged, LoggedCell, OpenedCell, SCAN, Valued, place_words, words_of,
};
#[path = "native_tables_columns.rs"]
mod native_tables_columns;

#[path = "native_tables_filters.rs"]
mod native_tables_filters;

/// The page's name, on its tab and its heading.
pub(crate) const TABLES_TITLE: &str = "Tables";

/// How often a shown page asks whether the registry moved, for what other Macs and verbs wrote;
/// it reads again only where it did ([`crate::tables_store::mark`]).
const READ_AGAIN: Duration = Duration::from_secs(30);

/// A column's group, in the grid's order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Group {
    /// The owner's cells on the owner's sheets.
    Stored,
    /// Cells set when the row is added, by whatever adds it, and fixed after.
    Born,
    /// Cells aterm keeps; every column of a sheet a Mac or a tool writes.
    SetByAterm,
    /// Every column of a reference sheet.
    Reference,
    /// How many rows of another table name this row.
    Linked,
}

impl Group {
    /// Its word over its columns. A sheet the rail lists under SET BY ATERM or REFERENCE puts
    /// all its columns under that word, so the two never disagree.
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Stored => "STORED",
            Self::Born => "SET WHEN ADDED",
            Self::SetByAterm => "SET BY ATERM",
            Self::Reference => "REFERENCE",
            Self::Linked => "LINKED ROWS",
        }
    }

    /// Every group, in the grid's order.
    const ALL: [Self; 5] = [
        Self::Stored,
        Self::Born,
        Self::SetByAterm,
        Self::Reference,
        Self::Linked,
    ];
}

/// Who wrote an operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// What kind of write an operation is, as `aterm-db` decides it (`aterm_db::att::EntryKind`).
/// Who typed an explicit write, a person or a program at that Mac, is said only by a version 2
/// record ([`Author::said`]).
pub(crate) enum AuthorKind {
    Explicit,
    Aterm,
    Import,
}

/// An operation's writer: what kind, from which Mac, and the time its writer claimed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Author {
    pub(crate) kind: AuthorKind,
    /// The writing Mac's name (`m73`), where its facts bind a machine row: that row's name as
    /// history gives it (`aterm_db::att::Mac::name`).
    pub(crate) label: Option<String>,
    /// The start of the writer's Commons machine id (of its actor where the roster binds no
    /// machine), which the page shows, dimmed, where no row binds.
    pub(crate) id: String,
    /// The time the writer's clock claimed, signed with the operation.
    pub(crate) at_ms: u64,
    /// The operation's place in the causal order: the latest claim among it and everything it
    /// saw, and a count that steps past ancestors with that same claim (`Handle::history`'s hlc).
    pub(crate) hlc: (u64, u64),
    /// Who made the change, why and on what evidence, where its record says (a version 2
    /// record, `aterm_db::att::Entry::said`): the signing Mac's statement.
    pub(crate) said: Option<aterm_db::said::Said>,
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
    /// `m73`, `aterm · m73`, `import · m73`, `claude · m73` where the record names who made it
    /// (its program, else what it said of itself; [`Self::statement`] has the whole principal);
    /// the short id in a label's place where none is known.
    pub(crate) fn who(&self) -> String {
        let name = self.label.as_deref().unwrap_or(&self.id);
        match (self.kind, self.principal()) {
            (AuthorKind::Import, _) => format!("import \u{b7} {name}"),
            (AuthorKind::Aterm, _) => format!("aterm \u{b7} {name}"),
            (AuthorKind::Explicit, Some(by)) => format!("{by} \u{b7} {name}"),
            (AuthorKind::Explicit, None) => name.to_owned(),
        }
    }

    /// Who the record says made the change, in a word (`claude`, `person`), where it says: its
    /// program, else, outside every aterm session, what it said of itself. In a session aterm
    /// read no program of (who acted there is unknown), the writer's own word is no attested
    /// name: only the `By:` line ([`Self::statement`]) shows it, beside the session.
    fn principal(&self) -> Option<String> {
        let principal = &self.said.as_ref()?.principal;
        principal
            .program
            .clone()
            .or_else(|| {
                principal
                    .session
                    .is_none()
                    .then(|| principal.said.clone())
                    .flatten()
            })
            .filter(|who| !who.is_empty())
    }

    /// The lines a history entry adds under its writer, where the record gives them: `By: …`,
    /// the whole principal (`claude s-4705…`), `Why: …` and one `Evidence: …` per piece.
    fn statement(&self) -> Vec<String> {
        let Some(said) = &self.said else {
            return Vec::new();
        };
        let mut lines = Vec::new();
        let who = said.who();
        if !who.is_empty() {
            lines.push(format!("By: {who}"));
        }
        if let Some(reason) = &said.reason {
            lines.push(format!("Why: {reason}"));
        }
        for piece in &said.evidence {
            lines.push(format!("Evidence: {}", piece.spell()));
        }
        lines
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
    /// When the Mac that states it says it saw it (an owned row's `at`): what a Mac states of
    /// itself is an observation, and this is when it was made.
    pub(crate) seen_ms: Option<u64>,
    /// The key a one-row reference holds, where the cell is one: `text` then says the words its
    /// row is known by ([`rows::Targets`]), and a Write, a copy and an open cell take this.
    pub(crate) refers: Option<String>,
    /// The grid's text of a value the page edits and `text` says in words (a time as its date):
    /// a Write, a copy and an open cell take this, and the cell bar names no key for it.
    pub(crate) holds: Option<String>,
}

impl Shown {
    /// What the cell holds as the grid text writes it: a reference's key, else the grid's text
    /// of a value said in words, else what it shows.
    pub(crate) fn value(&self) -> &str {
        self.refers
            .as_deref()
            .or(self.holds.as_deref())
            .unwrap_or(&self.text)
    }
}

/// What a Mac states of a cell of the owner's table, read beside what the cell holds: a stated
/// value and the newest observation are each shown, and neither is written over the other.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Observed {
    /// The value as the grid would show it.
    pub(crate) text: String,
    /// The Mac whose statement it is.
    pub(crate) by: Author,
    /// When that Mac says it saw it.
    pub(crate) at_ms: u64,
    /// The title of the table the statement is a row of (`Mac facts`).
    pub(crate) from: &'static str,
}

impl Observed {
    /// Whether this statement stands over `held`, another row's of the same cell: the later in
    /// the causal order (the operation's hlc) stands, and only between statements no order
    /// tells apart the later claimed time. A Mac whose clock runs ahead never holds the cell
    /// past a statement made after its own.
    // Read only by the unix-only table store (`tables_store::Read::observed`).
    #[cfg(unix)]
    pub(crate) fn supersedes(&self, held: &Self) -> bool {
        (self.by.hlc, self.at_ms) > (held.by.hlc, held.at_ms)
    }
}

/// How a column's cells are read when its rows are put in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(
    not(unix),
    expect(
        dead_code,
        reason = "a time or words column is made by the unix-only table store"
    )
)]
pub(crate) enum ColumnKind {
    /// A whole number: ordered by its value, and set at the right of its cell.
    Number,
    /// A time or a day, which the page shows as its date: ordered by its value.
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
    /// The compiled column: its type, which its header says and its open cell offers the
    /// choices of, and whether it may be empty; `None` for a column the page makes (a row's own
    /// key, the Mac that states a row, a count of linked rows) and one a newer build wrote.
    pub(crate) compiled: Option<&'static aterm_db::typed::Column>,
    /// The row's key is this column, or derives from it (a link's machine and account).
    pub(crate) key: bool,
}

/// The compiled table and collection a cells sheet was read from, with the table's cells as
/// that read found them: what the save's own reading of a grid
/// ([`aterm_db::typed::grid::plan`]) checks a staged cell against, and a new row's key is
/// derived from as the save derives it.
#[derive(Clone, Debug)]
pub(crate) struct Schema {
    pub(crate) compiled: &'static crate::tables_store::Compiled,
    pub(crate) collection: &'static aterm_db::typed::Collection,
    pub(crate) cells: std::sync::Arc<aterm_db::typed::cell::Cells>,
    /// How each claimed column settled in that read: what a label staged in one reads as once
    /// written is asked of it ([`claims`]).
    pub(crate) settled: std::sync::Arc<aterm_db::typed::claim::Settlements>,
}

impl PartialEq for Schema {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.compiled, other.compiled)
            && self.collection == other.collection
            && std::sync::Arc::ptr_eq(&self.cells, &other.cells)
            && std::sync::Arc::ptr_eq(&self.settled, &other.settled)
    }
}

impl Eq for Schema {}

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
    /// Each Mac's own rows ([`aterm_db::typed::Kind::Owned`]): read only, with no lifecycle,
    /// conflicts or staging.
    pub(crate) owned: bool,
    pub(crate) columns: Vec<SheetColumn>,
    /// The column that names a row (`label`, an address's `addr`): the grid keeps it at its
    /// left while the rest scroll across.
    pub(crate) key: String,
    pub(crate) rows: Vec<SheetRow>,
    /// Its operations, oldest first.
    pub(crate) written: Vec<Written>,
    /// In a read as of a past point, each cell that changed since, by row and column.
    pub(crate) since: BTreeMap<(String, String), Since>,
    /// What each Mac states of a cell, by row and column, where a Mac's own row states it.
    pub(crate) observed: BTreeMap<(String, String), Observed>,
    /// The export the grid shows: what a Write is compared against.
    pub(crate) base: Base,
    /// What a staged cell of a cells sheet is checked against as it is staged; `None` for an
    /// owned sheet, which stages nothing.
    pub(crate) schema: Option<Schema>,
    /// How its rows are added, retired, ended and restored ([`rows`]); `None` for a sheet with
    /// no lifecycle (an owned one), whose rows the page neither adds nor retires.
    pub(crate) life: Option<rows::RowLife>,
    /// The live rows of any table that name each row of this one, by its key: what a retire of
    /// it waits for (`aterm_db::typed::check::referrers`).
    pub(crate) referrers: BTreeMap<String, Vec<rows::Referrer>>,
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

/// Every listed collection, as one read found them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Snapshot {
    /// The rail: every collection of the generation read, as its compiled table lists them
    /// ([`crate::tables_store::listed`]).
    pub(crate) listed: Vec<crate::tables_store::Listed>,
    pub(crate) sheets: Vec<Sheet>,
    /// What check finds and the store's state ([`crate::native_tables_check`]), where a view
    /// showed Check when this read was made; `None` otherwise. An error says why it could not
    /// be read.
    pub(crate) check: Option<Box<Result<crate::native_tables_check::Checked, String>>>,
    /// The past point this read is at; `None` for the table as it is.
    pub(crate) at: Option<AsOf>,
    pub(crate) read_ms: u64,
    /// Why this Mac cannot write the registry, where it cannot
    /// (`aterm_db::att::Handle::cannot_write`): the page then takes no edit, and the Write bar's
    /// place says this.
    pub(crate) cannot_write: Option<String>,
    /// What the table read holds and does not project, in aterm-db's one sentence for it
    /// (`aterm_db::typed::View::refused_words`: how many, whose and why), where it holds any;
    /// with it each family after the first that could not be read, or written, here, and why
    /// ([`crate::tables_store::read`]): the page says it under its header. The read is complete
    /// all the same, and the page writes as it would.
    pub(crate) refused: Option<String>,
    /// The rows a reference may name, by collection: what a reference cell shows and chooses
    /// from ([`rows::Targets`]).
    pub(crate) targets: rows::Targets,
}

/// One collection's staged cells and the export they were staged against.
pub(crate) struct Staged {
    /// The sheet they were staged on, which what the Write did names its table by.
    pub(crate) sheet: &'static str,
    /// The generation the sheet was read from: the save is its family's.
    #[cfg_attr(
        not(unix),
        expect(dead_code, reason = "only the table store of macOS and Linux saves")
    )]
    pub(crate) compiled: &'static crate::tables_store::Compiled,
    pub(crate) base: Base,
    pub(crate) cells: BTreeMap<(String, String), String>,
    /// Its new rows, each by the key the page gives it until it is written ([`rows::NEW`]) with
    /// its typed cells by column, in the order they were added.
    pub(crate) born: Vec<(String, BTreeMap<String, String>)>,
}

/// What one Write did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Wrote {
    pub(crate) cells: usize,
    /// How many rows were born.
    pub(crate) added: usize,
    /// Each new row a table's save took, by its table, the key the page gave it
    /// ([`rows::NEW`]) and the key it was born under.
    pub(crate) born: Vec<(String, String, String)>,
    /// Cells another writer changed since the read: kept staged, never written over.
    pub(crate) kept: Vec<(String, String, String)>,
    /// Cells the table's rules or types refused, with the rule; nothing of that table was written.
    pub(crate) refused: Vec<(String, String, String, String)>,
    /// Tables whose save did not reach the registry, and why; nothing of them was written.
    pub(crate) failed: Vec<(String, String)>,
}

enum Work {
    /// Read again at the point the reader is at, where the registry moved since the last read.
    Read,
    /// Read one page of History, the registry's records newest first ([`history::registry`]).
    History(history::registry::Asked),
    /// Read what one record wrote, replaced, had not seen and what wrote its cells since.
    Record([u8; 32]),
    /// Whether a view of the page is shown: a hidden page's reader sleeps until one is.
    Shown(bool),
    /// Read as of an operation (the table as it is, where `None`), and stay there once read.
    At(Option<[u8; 32]>),
    Write(Vec<Staged>),
}

pub(crate) enum Reply {
    /// One fully projected sheet from the first read. The terminal read decides when it is writable.
    Opening {
        sheet: Box<Sheet>,
        targets: rows::Targets,
    },
    /// A read, and whether it answers a move to another point ([`Work::At`]).
    Read {
        read: Result<Snapshot, String>,
        moved: bool,
        /// The registry's revision as it was asked before the read (`None` where it could not
        /// be): History reads again only where it is another than History's.
        revision: Option<[u8; 32]>,
    },
    /// The registry had not moved when it was asked at this time (ms): the page shows what it
    /// showed, read as of then.
    Unchanged(u64),
    Wrote(Result<Wrote, String>),
    /// A page of History, answering the ask numbered `seq`.
    History {
        seq: u64,
        read: Result<history::registry::Paged, String>,
    },
    /// What record `operation` wrote, opened.
    Record {
        operation: [u8; 32],
        read: Result<history::registry::Opened, String>,
    },
}

/// The Tables reader's next work: the first of `queued`, else the channel's (a read again where
/// `again` passes with none, `None` once the page is gone). A read of History that a later one
/// queued behind it stands for is dropped unread: the page takes only the answer to its last ask
/// ([`history::registry::Registry::take_page`]), so a word typed into Find a key at a time reads
/// the registry once, not once a key. Every other ask is done, in the order asked.
fn next_work(
    rx: &mpsc::Receiver<Work>,
    queued: &mut std::collections::VecDeque<Work>,
    again: Option<Duration>,
) -> Option<Work> {
    loop {
        let work = match queued.pop_front() {
            Some(work) => work,
            None => match again {
                Some(again) => match rx.recv_timeout(again) {
                    Ok(work) => work,
                    Err(mpsc::RecvTimeoutError::Timeout) => Work::Read,
                    Err(mpsc::RecvTimeoutError::Disconnected) => return None,
                },
                None => rx.recv().ok()?,
            },
        };
        if matches!(work, Work::History(_)) {
            queued.extend(rx.try_iter());
            if queued.iter().any(|later| matches!(later, Work::History(_))) {
                continue;
            }
        }
        return Some(work);
    }
}

/// A staged cell's address: collection, row, column.
type At = (String, String, String);

/// One change check of the page's reader, as a test reads it ([`TablesApp::checks`]): the
/// registry's revision the reader's kept sighting was of as the check was asked (`held`) and
/// once it answered (`kept`), and the revision of the History the reader then kept (`history`).
/// A check asked with `held` the registry's revision is the one that opens no store
/// (`aterm_db::att::Handle::revision_kept`).
#[cfg(all(test, unix))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Check {
    pub(crate) held: Option<[u8; 32]>,
    pub(crate) kept: Option<[u8; 32]>,
    pub(crate) history: Option<[u8; 32]>,
}

pub(crate) struct TablesApp {
    work: mpsc::Sender<Work>,
    replies: mpsc::Receiver<Reply>,
    snapshot: Option<Snapshot>,
    opening_sheets: BTreeMap<&'static str, Sheet>,
    opening_targets: rows::Targets,
    opening_wake: Arc<AtomicBool>,
    wanted: Arc<Mutex<&'static str>>,
    initial_collections: Vec<crate::tables_store::Listed>,
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
    /// The rule the save's own reading of the grid refuses a staged cell by, worked out as it is
    /// staged ([`crate::tables_store::refusals`]), and what it was worked out from: the read and
    /// the staged cells.
    checked: BTreeMap<At, String>,
    checked_of: Option<(u64, BTreeMap<At, String>)>,
    /// Each staged label another row claims, as the read settles it once written ([`claims`]),
    /// worked out with `checked`.
    claimed: BTreeMap<At, claims::Claimed>,
    /// Point reads queued on the worker, including explicit refreshes at the current head.
    asked: usize,
    /// How many of those FIFO replies must arrive before every requested time change is
    /// settled. Requests after that one only refresh the head; other views can keep editing.
    moving: usize,
    /// Whether the last requested point failed to read, leaving its previous snapshot in place.
    move_failed: bool,
    /// Whether the reader was last told a view of the page is shown.
    shown: bool,
    /// How many reads the page has taken: a view works its rows' order out once for each
    /// ([`TablesViewState::shown_rows`]).
    reads: u64,
    /// Whether a view shows Check: the reader works check out only while one does
    /// ([`crate::native_tables_check`]).
    checking: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Each change check the reader has made since a test last took them, in order.
    #[cfg(all(test, unix))]
    checks: std::sync::Arc<std::sync::Mutex<Vec<Check>>>,
    /// The new rows not written, by sheet, each under the key the page gives it ([`rows::NEW`]),
    /// in the order they were added: each read shows them under its sheet's rows.
    born: Vec<(&'static str, String)>,
    /// How many new rows the page has keyed: each gets the next number.
    born_count: u64,
    /// The new rows the Write in flight carried.
    sent_born: Vec<(&'static str, String)>,
    /// What History, the drawer's Registry scope, has read of the registry's records.
    registry: history::registry::Registry,
    /// Each new row's key as the save would derive it from its staged cells now, where the
    /// save's reading of them refuses nothing ([`crate::tables_store::Reading::keys`]).
    born_keys: BTreeMap<(&'static str, String), String>,
}

impl TablesApp {
    /// The page's model, reading the registry at `paths` (this Mac's, where `None`) on its own
    /// thread, which calls `wake` after each answer.
    ///
    /// # Errors
    /// The thread cannot start.
    pub(crate) fn new(
        paths: Option<crate::tables_store::Paths>,
        families: &'static [&'static crate::tables_store::Compiled],
        wake: impl Fn() + Send + 'static,
    ) -> Result<Self, String> {
        Self::paced(paths, families, wake, Some(READ_AGAIN))
    }

    /// [`Self::new`] whose shown reader asks again every `again`, or only when asked where
    /// `None` (a test pairing each ask with its answer).
    fn paced(
        paths: Option<crate::tables_store::Paths>,
        families: &'static [&'static crate::tables_store::Compiled],
        wake: impl Fn() + Send + 'static,
        again: Option<Duration>,
    ) -> Result<Self, String> {
        let (work_tx, work_rx) = mpsc::channel::<Work>();
        let (reply_tx, reply_rx) = mpsc::channel();
        let checking = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let check = checking.clone();
        let initial_collections = crate::tables_store::initial_collections(families);
        let wanted = Arc::new(Mutex::new(
            initial_collections.first().map_or("", |entry| entry.name),
        ));
        let reader_wanted = Arc::downgrade(&wanted);
        let opening_wake = Arc::new(AtomicBool::new(false));
        let wake_pending = Arc::clone(&opening_wake);
        #[cfg(all(test, unix))]
        let checks = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        #[cfg(all(test, unix))]
        let checked = checks.clone();
        crate::qos::spawn_lane_thread("aterm-tables", crate::qos::Role::Background, move || {
            let paths = match paths {
                Some(paths) => Ok(paths),
                None => crate::tables_store::live(),
            };
            // The point the page shows: a read at another point moves it only once it reads.
            let mut point = None;
            // What the last read at `point` rested on (the registry's mark and whether it worked
            // check out); `None` until a read succeeds there.
            let mut read_at = None;
            let mut first_read = true;
            let mut shown = true;
            // The asks taken off the channel to look behind one, not yet done.
            let mut queued = std::collections::VecDeque::new();
            // What this reader keeps of the registry from one read to the next: History's order
            // at the registry's revision (Read older and a filter changed at that revision read
            // no record again), and what its last open read of the registry's files, which the
            // mark below and History's pages both ask to learn the registry unmoved without
            // opening it. Until History is read it holds no record.
            let mut kept = crate::tables_store::Kept::default();
            loop {
                // A hidden page's reader asks nothing until a view of it is shown again.
                let Some(work) = next_work(&work_rx, &mut queued, again.filter(|_| shown)) else {
                    return;
                };
                if let Work::Shown(now) = work {
                    let was = std::mem::replace(&mut shown, now);
                    if !now || was {
                        continue;
                    }
                }
                let paths = match &paths {
                    Ok(paths) => paths,
                    Err(why) => {
                        let reply = match work {
                            Work::History(asked) => Reply::History {
                                seq: asked.seq,
                                read: Err(why.clone()),
                            },
                            Work::Record(operation) => Reply::Record {
                                operation,
                                read: Err(why.clone()),
                            },
                            work => Reply::Read {
                                read: Err(why.clone()),
                                moved: matches!(work, Work::At(_)),
                                revision: None,
                            },
                        };
                        if reply_tx.send(reply).is_err() {
                            return;
                        }
                        wake();
                        continue;
                    }
                };
                // History's reads stand apart from the point the page shows: they list every
                // record, whatever point the tables are read at.
                let work = match work {
                    Work::History(asked) => {
                        let read = crate::tables_store::history(paths, families, &asked, &mut kept);
                        let reply = Reply::History {
                            seq: asked.seq,
                            read,
                        };
                        if reply_tx.send(reply).is_err() {
                            return;
                        }
                        wake();
                        continue;
                    }
                    Work::Record(operation) => {
                        let read = crate::tables_store::record(paths, families, operation);
                        if reply_tx.send(Reply::Record { operation, read }).is_err() {
                            return;
                        }
                        wake();
                        continue;
                    }
                    work => work,
                };
                let mut at = point;
                let moved = matches!(work, Work::At(_));
                let again = matches!(work, Work::Read | Work::Shown(_));
                let opening = first_read && again;
                match work {
                    Work::Write(staged) => {
                        let wrote = crate::tables_store::write(paths, staged);
                        if reply_tx.send(Reply::Wrote(wrote)).is_err() {
                            return;
                        }
                    }
                    Work::At(asked) => at = asked,
                    Work::Read | Work::Shown(_) | Work::History(_) | Work::Record(_) => {}
                }
                let checking = check.load(std::sync::atomic::Ordering::Relaxed);
                // Asked before the read, so a write landing during it moves the next mark. A
                // read rests on the mark and on whether it worked check out: Check shown on an
                // unmoved registry still reads, so its findings are worked out. One that worked
                // check out rests on this Mac's hearing ledger too, which Check's sentences on
                // the registry's checkpoints are judged by and no record of the registry moves.
                #[cfg(all(test, unix))]
                let held = kept.sighted().map(|revision| revision.0);
                let rests = crate::tables_store::mark(paths, families, &mut kept)
                    .ok()
                    .map(|mark| {
                        let heard = checking
                            .then(|| crate::tables_store::fence_mark(paths))
                            .flatten();
                        (mark, checking, heard)
                    });
                #[cfg(all(test, unix))]
                if let Ok(mut checked) = checked.lock() {
                    checked.push(Check {
                        held,
                        kept: kept.sighted().map(|revision| revision.0),
                        history: kept.revision().map(|revision| revision.0),
                    });
                }
                let revision = rests.as_ref().map(|(mark, ..)| mark.digest());
                if again && rests.is_some() && rests == read_at {
                    let now = aterm_time::unix_millis().unwrap_or(0);
                    if reply_tx.send(Reply::Unchanged(now)).is_err() {
                        return;
                    }
                    wake();
                    continue;
                }
                let result = if opening && at.is_none() && !checking {
                    let wanted = || {
                        reader_wanted.upgrade().map(|wanted| {
                            *wanted
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                        })
                    };
                    let mut publish = |sheet, targets| {
                        if reply_tx
                            .send(Reply::Opening {
                                sheet: Box::new(sheet),
                                targets,
                            })
                            .is_ok()
                            && !wake_pending.swap(true, Ordering::AcqRel)
                        {
                            wake();
                        }
                    };
                    let mut opening = crate::tables_store::Opening {
                        wanted: &wanted,
                        publish: &mut publish,
                    };
                    crate::tables_store::read_opening(paths, families, &mut opening)
                } else {
                    crate::tables_store::read(paths, families, at, checking)
                };
                let read = match result {
                    Ok((snapshot, whole)) => {
                        first_read = false;
                        point = at;
                        // A read that showed a mask in place of a number it could not open yet
                        // is read again, since that number may open with the registry unmoved.
                        read_at = rests.filter(|_| whole);
                        Ok(snapshot)
                    }
                    Err(why) => {
                        read_at = None;
                        Err(why)
                    }
                };
                let reply = Reply::Read {
                    read,
                    moved,
                    revision,
                };
                if reply_tx.send(reply).is_err() {
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
            opening_sheets: BTreeMap::new(),
            opening_targets: rows::Targets::new(),
            opening_wake,
            initial_collections,
            wanted,
            unread: None,
            writing: false,
            note: None,
            staged: BTreeMap::new(),
            seen: BTreeMap::new(),
            sent: BTreeMap::new(),
            refused: BTreeMap::new(),
            checked: BTreeMap::new(),
            checked_of: None,
            claimed: BTreeMap::new(),
            asked: 0,
            moving: 0,
            move_failed: false,
            shown: true,
            reads: 0,
            checking,
            #[cfg(all(test, unix))]
            checks,
            born: Vec::new(),
            born_count: 0,
            sent_born: Vec::new(),
            born_keys: BTreeMap::new(),
            registry: history::registry::Registry::default(),
        };
        let _ = app.work.send(Work::Read);
        Ok(app)
    }

    /// Tell the reader whether a view of the page is shown: a hidden page's reader asks nothing
    /// of the registry, and one shown again reads at once where the registry moved meanwhile.
    pub(crate) fn set_shown(&mut self, shown: bool) {
        if self.shown != shown {
            self.shown = shown;
            let _ = self.work.send(Work::Shown(shown));
        }
    }

    /// Take every answer the reader sent; whether the page changed.
    pub(crate) fn poll(&mut self) -> bool {
        // Re-arm before draining. A concurrent delivery is either drained now or wakes us again.
        self.opening_wake.store(false, Ordering::Release);
        let mut changed = false;
        while let Ok(reply) = self.replies.try_recv() {
            self.take(reply);
            changed = true;
        }
        changed
    }

    /// Take the reader's next terminal answer, applying any opening sheets along the way.
    #[cfg(all(test, unix))]
    pub(crate) fn settle(&mut self) {
        loop {
            let reply = self
                .replies
                .recv_timeout(Duration::from_secs(60))
                .expect("the Tables reader answers within a minute");
            let opening = matches!(reply, Reply::Opening { .. });
            self.take(reply);
            if !opening {
                return;
            }
        }
    }

    /// The change checks the reader made since this was last asked, in order. Each is recorded
    /// before its answer is sent, so the checks of every answer taken are here.
    #[cfg(all(test, unix))]
    pub(crate) fn checks(&self) -> Vec<Check> {
        self.checks
            .lock()
            .map(|mut checks| std::mem::take(&mut *checks))
            .unwrap_or_default()
    }

    /// Take answers until the Write's and the read after it: a test's hang detector.
    #[cfg(all(test, unix))]
    pub(crate) fn settle_write(&mut self) {
        // A reader that reads again on its own answers forever: with no Write out, no answer
        // ends the wait.
        assert!(
            self.writing,
            "a Write is out: {}",
            self.note.as_deref().unwrap_or("nothing was sent")
        );
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
            revision: None,
        });
    }

    fn take(&mut self, reply: Reply) {
        match reply {
            Reply::Opening { sheet, targets } => {
                if self.snapshot.is_none() {
                    self.opening_targets.extend(targets);
                    self.opening_sheets.insert(sheet.name, *sheet);
                    self.unread = None;
                }
            }
            Reply::Read {
                read: Ok(mut snapshot),
                moved,
                revision,
            } => {
                if !self.opening_sheets.is_empty() {
                    let mut sheets = std::mem::take(&mut self.opening_sheets);
                    sheets.extend(snapshot.sheets.drain(..).map(|sheet| (sheet.name, sheet)));
                    snapshot.sheets = snapshot
                        .listed
                        .iter()
                        .filter_map(|entry| sheets.remove(entry.name))
                        .collect();
                }
                self.opening_targets.clear();
                self.snapshot = Some(snapshot);
                self.unread = None;
                self.reads = self.reads.wrapping_add(1);
                if moved {
                    self.asked = self.asked.saturating_sub(1);
                    self.moving = self.moving.saturating_sub(1);
                    self.move_failed = false;
                }
                self.show_born();
                self.drop_stranded();
                self.recheck();
                // History reads again where this read found the registry at another revision.
                self.registry_moved(revision);
            }
            Reply::History { seq, read } => self.registry_take(seq, read),
            Reply::Record { operation, read } => {
                self.registry.take_record(operation, read);
            }
            Reply::Read {
                read: Err(why),
                moved,
                ..
            } => {
                if !self.opening_sheets.is_empty() {
                    self.reads = self.reads.wrapping_add(1);
                }
                self.opening_sheets.clear();
                self.opening_targets.clear();
                if moved {
                    self.asked = self.asked.saturating_sub(1);
                    self.moving = self.moving.saturating_sub(1);
                    self.move_failed = true;
                }
                if self.snapshot.is_none() {
                    self.unread = Some(why);
                } else if moved {
                    self.note = Some(format!("Not read: {why}"));
                }
            }
            Reply::Unchanged(checked_ms) => {
                if let Some(snapshot) = &mut self.snapshot {
                    snapshot.read_ms = checked_ms;
                }
            }
            Reply::Wrote(wrote) => {
                self.writing = false;
                match wrote {
                    Ok(wrote) => self.wrote(wrote),
                    Err(why) => self.note = Some(format!("Not written: {why}")),
                }
                self.sent.clear();
                self.recheck();
            }
        }
    }

    /// Drop each staged cell the read just taken can no longer write (its table, its live row
    /// or its column is gone, or the cell no longer takes an edit), and say how many: a cell
    /// left staged there would never be written, and would hold the page at this point.
    fn drop_stranded(&mut self) {
        let stranded: Vec<At> = self
            .staged
            .keys()
            .filter(|(sheet, pk, column)| !self.writable(sheet, pk, column))
            .cloned()
            .collect();
        if stranded.is_empty() {
            return;
        }
        for at in &stranded {
            self.unstage(at);
        }
        self.note = Some(match stranded.len() {
            1 => "1 change not written was dropped: its cell is no longer in the table".to_owned(),
            n => format!(
                "{n} changes not written were dropped: their cells are no longer in the table"
            ),
        });
    }

    /// Whether a Write can carry a cell staged at `sheet`, `pk`, `column`: the page edits it
    /// ([`Self::takes`], or it is a row's lifecycle the pane stages), and its row is one of that
    /// export's, or a new row the page holds.
    fn writable(&self, sheet: &str, pk: &str, column: &str) -> bool {
        (self.takes(sheet, pk, column) || self.stages_life(sheet, pk, column))
            && self
                .sheet(sheet)
                .is_some_and(|s| s.rows.iter().any(|r| r.pk == pk))
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
        // A new row the Write carried is now a row of the table, under its own key, unless its
        // table was not written. A cell of it staged while the Write was out is an edit of that
        // row now, never a row of its own.
        let sent_born = std::mem::take(&mut self.sent_born);
        self.born.retain(|(sheet, pk)| {
            !sent_born.iter().any(|(s, p)| s == sheet && p == pk)
                || unwritten.iter().any(|c| c.as_str() == *sheet)
        });
        for (table, new, pk) in &wrote.born {
            let late: Vec<At> = self
                .staged
                .keys()
                .filter(|(s, p, _)| s == table && p == new)
                .cloned()
                .collect();
            for at in late {
                if let Some(text) = self.staged.remove(&at) {
                    self.seen.remove(&at);
                    self.staged.insert((at.0, pk.clone(), at.2), text);
                }
            }
        }
        self.show_born();
        self.refused = wrote
            .refused
            .iter()
            .map(|(c, p, k, rule)| ((c.clone(), p.clone(), k.clone()), rule.clone()))
            .collect();
        let mut words = vec![match wrote.cells {
            1 => "Wrote 1 cell".to_owned(),
            n => format!("Wrote {n} cells"),
        }];
        match wrote.added {
            0 => {}
            1 => words.push("added 1 row".to_owned()),
            n => words.push(format!("added {n} rows")),
        }
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
            let title = self
                .sheet(table)
                .map_or(table.as_str(), |sheet| sheet.title);
            words.push(format!("{title} not written: {why}"));
        }
        self.recheck();
        words.extend(self.held_back());
        self.note = Some(words.join(" \u{b7} "));
    }

    /// The past point the page shows, where it shows one.
    fn as_of(&self) -> Option<&AsOf> {
        self.snapshot.as_ref().and_then(|s| s.at.as_ref())
    }

    /// Whether the page may move to another point: nothing staged, added or being written, which
    /// a past table could not take.
    fn movable(&self) -> bool {
        self.snapshot.is_some() && self.staged.is_empty() && self.born.is_empty() && !self.writing
    }

    /// Whether the page takes no edit: it shows a past point, or is moving to another one, or
    /// this Mac cannot write the registry ([`Self::cannot_write`]).
    fn fixed(&self) -> bool {
        self.snapshot.is_none()
            || self.moving > 0
            || self.as_of().is_some()
            || self.cannot_write().is_some()
    }

    /// Why this Mac cannot write the registry, as the last read found it, where it cannot.
    pub(crate) fn cannot_write(&self) -> Option<&str> {
        self.snapshot
            .as_ref()
            .and_then(|s| s.cannot_write.as_deref())
    }

    /// What the last read holds and does not project ([`Snapshot::refused`]), where it holds any.
    pub(crate) fn refused(&self) -> Option<&str> {
        self.snapshot.as_ref().and_then(|s| s.refused.as_deref())
    }

    /// Read the table as of `at` (as it is, where `None`); with cells not written, say why not.
    fn go(&mut self, at: Option<[u8; 32]>) {
        if self.snapshot.is_none() {
            return;
        }
        if !self.movable() {
            self.note = Some("Write or discard the changes not written first".to_owned());
            return;
        }
        if let Err(why) = self.ask_point(at, true) {
            self.note = Some(why);
        }
    }

    fn ask_point(&mut self, at: Option<[u8; 32]>, changes_point: bool) -> Result<(), String> {
        self.work
            .send(Work::At(at))
            .map_err(|_| "The Tables reader is unavailable".to_owned())?;
        self.asked += 1;
        if changes_point {
            self.moving = self.asked;
        }
        self.note = None;
        Ok(())
    }

    fn sheet(&self, name: &str) -> Option<&Sheet> {
        match &self.snapshot {
            Some(snapshot) => snapshot.sheets.iter().find(|sheet| sheet.name == name),
            None => self.opening_sheets.get(name),
        }
    }

    fn targets(&self) -> &rows::Targets {
        self.snapshot
            .as_ref()
            .map_or(&self.opening_targets, |snapshot| &snapshot.targets)
    }

    fn prioritize(&self, name: &'static str) {
        if self.snapshot.is_none() {
            *self
                .wanted
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = name;
        }
    }

    /// The sheet the last read holds of collection `name`.
    pub(crate) fn sheet_named(&self, name: &str) -> Option<&Sheet> {
        self.sheet(name)
    }

    /// The names of the collections the rail offers.
    pub(crate) fn listed_names(&self) -> Vec<&'static str> {
        self.listed().iter().map(|listed| listed.name).collect()
    }

    /// The title the rail gives collection `name` (`Machines`), where it lists it.
    pub(crate) fn title_of(&self, name: &str) -> Option<&'static str> {
        self.listed()
            .iter()
            .find(|listed| listed.name == name)
            .map(|listed| listed.title)
            .or_else(|| self.opening_sheets.get(name).map(|sheet| sheet.title))
    }

    /// The words row `pk` of collection `name` is known by, where the last read holds it: as a
    /// reference to it reads where its row may change ([`rows::Target::short`]), else as its
    /// pane names it ([`rows::row_name`]).
    pub(crate) fn row_words(&self, name: &str, pk: &str) -> Option<String> {
        let target = self
            .targets()
            .get(name)
            .and_then(|named| named.rows.get(pk));
        if let Some(target) = target {
            return Some(target.short.clone());
        }
        let sheet = self.sheet(name)?;
        let row = sheet.rows.iter().find(|row| row.pk == pk)?;
        rows::row_name(self, sheet, row, None)
    }

    /// Whether a view shows Check: on, the reader works check out at each read from now, and
    /// reads at once where the last read holds none; off, it stops.
    pub(crate) fn check(&mut self, on: bool) {
        self.checking
            .store(on, std::sync::atomic::Ordering::Relaxed);
        if on && self.checked_read().is_none() {
            let _ = self.work.send(Work::Read);
        }
    }

    /// What the last read found of check, where it was worked out.
    pub(crate) fn checked_read(
        &self,
    ) -> Option<&Result<crate::native_tables_check::Checked, String>> {
        self.snapshot.as_ref().and_then(|s| s.check.as_deref())
    }

    /// [`Self::checked_read`], where it was read.
    pub(crate) fn checked(&self) -> Option<&crate::native_tables_check::Checked> {
        self.checked_read().and_then(|read| read.as_ref().ok())
    }

    /// The active generation's collections once read. Until then, the schema alone supplies
    /// the collections available in every generation on platforms that host the registry.
    fn listed(&self) -> &[crate::tables_store::Listed] {
        match &self.snapshot {
            Some(snapshot) => &snapshot.listed,
            None => &self.initial_collections,
        }
    }

    /// The selected collection, or the first one offered. Before the first read a selection
    /// names the collection whose rows the view will show when they arrive.
    fn current(&self, view: &TablesViewState) -> Option<&'static str> {
        if self.snapshot.is_none()
            && (self.opening_sheets.contains_key(view.sheet)
                || view
                    .pending_row
                    .as_ref()
                    .is_some_and(|row| row.sheet == view.sheet))
        {
            return Some(view.sheet);
        }
        let listed = self.listed();
        listed
            .iter()
            .find(|entry| entry.name == view.sheet)
            .or_else(|| listed.first())
            .map(|entry| entry.name)
    }

    /// The one rule for whether the page edits a cell: its column is the owner's and the grid
    /// does not hold it read only ([`SheetColumn::editable`]), and this build can read what it
    /// holds. An owned sheet's columns are a Mac's own statement, so none of its cells is
    /// edited, staged, emptied, pasted into or kept.
    fn takes(&self, sheet: &str, pk: &str, column: &str) -> bool {
        let Some(sheet) = self.sheet(sheet) else {
            return false;
        };
        // A new row is typed where the grid types one: every column it does not hold read only.
        if rows::is_new(pk) {
            return sheet
                .life
                .as_ref()
                .is_some_and(|life| life.born_columns.iter().any(|c| c == column));
        }
        !sheet.owned
            && sheet.columns.iter().any(|c| c.name == column && c.editable)
            && !sheet
                .rows
                .iter()
                .find(|r| r.pk == pk)
                .and_then(|r| r.cells.get(column))
                .is_some_and(|c| c.raw)
    }

    fn staged_in(&self, sheet: &str) -> usize {
        self.staged.keys().filter(|(c, ..)| c == sheet).count()
    }

    /// Check each staged cell by the rule the save reads its grid with, where the read or the
    /// staged cells moved since the last check: each sheet's staged cells at once, as one Write
    /// would carry them.
    fn recheck(&mut self) {
        if self
            .checked_of
            .as_ref()
            .is_some_and(|(reads, staged)| *reads == self.reads && *staged == self.staged)
        {
            return;
        }
        let mut checked = BTreeMap::new();
        let mut born_keys = BTreeMap::new();
        let mut records = claims::Records::new();
        if let Some(snapshot) = &self.snapshot {
            for sheet in &snapshot.sheets {
                let cells: BTreeMap<(String, String), String> = self
                    .staged
                    .iter()
                    .filter(|((name, ..), _)| name == sheet.name)
                    .map(|((_, pk, column), text)| ((pk.clone(), column.clone()), text.clone()))
                    .collect();
                if cells.is_empty() {
                    continue;
                }
                let reading = crate::tables_store::refusals(sheet, &cells);
                for ((pk, column), rule) in reading.refused {
                    checked.insert((sheet.name.to_owned(), pk, column), rule);
                }
                for (new, key) in reading.keys {
                    born_keys.insert((sheet.name, new), key);
                }
                if let Some(carried) = reading.records {
                    records.insert(sheet.name, carried);
                }
            }
        }
        // What the save's rules for references refuse, which no one table's grid can see.
        for (at, rule) in self.reference_refusals() {
            checked.entry(at).or_insert(rule);
        }
        // A new row's key cannot be made while a cell it is made from is refused: that cell says
        // why, once, and the row's lifecycle says what stops its key only when nothing else does.
        let refused_cells: BTreeSet<(String, String)> = checked
            .keys()
            .filter(|(_, pk, column)| rows::is_new(pk) && column != aterm_db::typed::LIFECYCLE)
            .map(|(sheet, pk, _)| (sheet.clone(), pk.clone()))
            .collect();
        checked.retain(|(sheet, pk, column), _| {
            column != aterm_db::typed::LIFECYCLE
                || !refused_cells.contains(&(sheet.clone(), pk.clone()))
        });
        self.checked = checked;
        self.born_keys = born_keys;
        self.claimed = claims::claimed(self, &records);
        self.checked_of = Some((self.reads, self.staged.clone()));
    }

    /// Why the cell at `at` cannot be written: the rule its staged value breaks, else the one a
    /// Write was refused by.
    pub(crate) fn refusal(&self, at: &At) -> Option<&String> {
        self.checked.get(at).or_else(|| self.refused.get(at))
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
        if self.fixed() || !self.takes(sheet, pk, column) {
            return;
        }
        let at = (sheet.to_owned(), pk.to_owned(), column.to_owned());
        let shown = self
            .sheet(sheet)
            .and_then(|s| s.rows.iter().find(|r| r.pk == pk))
            .and_then(|r| r.cells.get(column));
        // A new row's cell left empty is one it was never given.
        let written = if rows::is_new(pk) {
            text.trim().is_empty()
        } else {
            shown.is_some_and(|c| c.values.is_empty() && c.value() == text)
        };
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
    /// values it shows as they are stored, a row's lifecycle among them (no cell types it, but
    /// a conflict in it is settled only by keeping one of its values).
    fn keepable(&self, sheet: &str, pk: &str, column: &str) -> bool {
        !self.fixed() && (self.takes(sheet, pk, column) || self.stages_life(sheet, pk, column))
    }

    /// Write the staged cells, each table's in one save; a table with a staged cell the check
    /// refuses is held back whole, since its save would refuse every cell of it, and says so.
    fn write(&mut self) {
        if self.staged.is_empty() || self.writing || self.fixed() {
            return;
        }
        self.recheck();
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let held: BTreeSet<&str> = self.checked.keys().map(|(c, ..)| c.as_str()).collect();
        let mut staged: Vec<Staged> = Vec::new();
        let mut sent = BTreeMap::new();
        let mut sent_born = Vec::new();
        for (at, text) in &self.staged {
            let (collection, pk, column) = at;
            if held.contains(collection.as_str()) {
                continue;
            }
            let Some(sheet) = snapshot.sheets.iter().find(|s| s.name == collection) else {
                continue;
            };
            // Only a cells sheet stages, and its read names the family its save is of.
            let Some(schema) = &sheet.schema else {
                continue;
            };
            let index = match staged.iter().position(|s| s.sheet == collection.as_str()) {
                Some(index) => index,
                None => {
                    staged.push(Staged {
                        sheet: sheet.name,
                        compiled: schema.compiled,
                        base: sheet.base.clone(),
                        cells: BTreeMap::new(),
                        born: Vec::new(),
                    });
                    staged.len() - 1
                }
            };
            // A new row's cells go as one row of the grid, its key left for the save to derive.
            if rows::is_new(pk) {
                let born = &mut staged[index].born;
                let row = match born.iter().position(|(key, _)| key == pk) {
                    Some(row) => row,
                    None => {
                        born.push((pk.clone(), BTreeMap::new()));
                        sent_born.push((sheet.name, pk.clone()));
                        born.len() - 1
                    }
                };
                born[row].1.insert(column.clone(), text.clone());
                sent.insert(at.clone(), text.clone());
                continue;
            }
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
        let held_back = self.held_back();
        if staged.is_empty() {
            self.note = held_back;
            return;
        }
        rows::order(self, &mut staged);
        if self.work.send(Work::Write(staged)).is_ok() {
            self.writing = true;
            self.sent = sent;
            self.sent_born = sent_born;
            self.note = None;
        }
    }

    /// What a Write holds back: each table with a refused cell, and how many it has.
    fn held_back(&self) -> Option<String> {
        let mut tables: BTreeMap<&str, usize> = BTreeMap::new();
        for (table, ..) in self.checked.keys() {
            *tables.entry(table.as_str()).or_default() += 1;
        }
        let words: Vec<String> = tables
            .into_iter()
            .map(|(table, n)| {
                let title = self.sheet(table).map_or(table, |sheet| sheet.title);
                let cells = if n == 1 { "cell" } else { "cells" };
                format!("{title} held back: {n} refused {cells}")
            })
            .collect();
        (!words.is_empty()).then(|| words.join(" \u{b7} "))
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
    /// The list of the column's words an enum's or a set of enums' cell opens on.
    pub(crate) choice: Option<Choice>,
}

/// The rows a sheet shows, top down, worked out once for each read, find, sort, lifecycle filter
/// and set of filters on its values: a staged cell, a key and a redraw leave it as it is, so a
/// row never moves under the selection, but for a row the lifecycle filter leaves out, which
/// shows while a change to it is staged.
#[derive(Default)]
struct Order {
    /// What it was worked out from: the sheet, the read ([`TablesApp::reads`]), the find's
    /// words, the sorted column with its direction, the lifecycle filter, the rows it leaves out
    /// that a staged change shows, and the filters on the columns' values.
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

type OrderOf = (
    &'static str,
    u64,
    String,
    Option<(String, bool)>,
    rows::Filter,
    BTreeSet<String>,
    native_tables_filters::Filters,
);

/// A range's other corner: the cell it began at, among the rows as they stood then.
struct Corner {
    pk: String,
    column: String,
    /// The [`Order::stamp`] the range was grown on. Once the rows shown or their order change,
    /// the rows between two corners are other rows, so the range is the one selected cell again.
    order: u64,
}

struct PendingRow {
    sheet: &'static str,
    key: String,
    column: Option<String>,
    /// An external record route always names the current table.
    current_record: bool,
}

pub(crate) struct TablesViewState {
    pub(crate) common: CommonViewState,
    sheet: &'static str,
    /// A row to reveal when its sheet arrives on opening or after returning to the head.
    pending_row: Option<PendingRow>,
    /// Why a record requested before its read finished could not be shown.
    missing_record: Option<String>,
    /// This view navigated while the first read was still delivering its sheets.
    opening_selection: bool,
    row: Option<String>,
    column: Option<String>,
    editing: Option<Editing>,
    find: TextInputState,
    /// How far the grid's rows are scrolled: a cell, so a read that moves the selected row
    /// brings it back into view as the rows are put in order ([`Self::put_in_order`]).
    scroll: Cell<f32>,
    across: f32,
    pub(crate) pane_scroll: f32,
    /// Under 900 px of width: whether the table picker lists the tables in the grid's place.
    pub(crate) picking: bool,
    /// Whether the page was last laid out under 900 px, where the picker stands for the rail:
    /// a choice from it leaves the keys on the picker, the control still drawn.
    pub(crate) narrow: Cell<bool>,
    /// The Check view ([`crate::native_tables_check`]).
    pub(crate) check: crate::native_tables_check::CheckView,
    /// Which columns the grid draws, and the list that chooses them ([`native_tables_columns`]).
    columns: native_tables_columns::ColumnsView,
    /// Which rows the grid shows by the values of their columns, and the list that chooses them
    /// ([`native_tables_filters`]).
    filters: native_tables_filters::FiltersView,
    /// The pane's Keep and Undo buttons, top down, as it last laid them out, each with where it
    /// stands in the pane's list: the arrows move among them, scrolling the one reached into
    /// view.
    pane_keys: std::cell::RefCell<Vec<(String, f32, f32)>>,
    /// How tall the pane's list is: what of it shows.
    pane_room: Cell<f32>,
    /// How far the rail's list of tables is scrolled, how far it can scroll, how tall its room
    /// is, and where each table's line stands in it: a short window still reaches every table.
    rail_scroll: f32,
    rail_limit: Cell<f32>,
    rail_room: Cell<f32>,
    rail_spans: std::cell::RefCell<Vec<(String, f32, f32)>>,
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
    /// The history drawer under the grid: open or closed, its scope, its list's scroll and the
    /// records opened in it. View state, kept nowhere and written nowhere.
    history: history::Drawer,
    /// Which rows each sheet shows by their lifecycle ([`rows::Filter`]): view state, kept
    /// across reads. A sheet not named shows its live rows.
    pub(crate) life: BTreeMap<&'static str, rows::Filter>,
}

impl Default for TablesViewState {
    fn default() -> Self {
        Self {
            common: CommonViewState::default(),
            // None yet: the view shows the first sheet the rail lists until one is chosen.
            sheet: "",
            pending_row: None,
            missing_record: None,
            opening_selection: false,
            row: None,
            column: None,
            editing: None,
            find: TextInputState::new(String::new()),
            scroll: Cell::new(0.0),
            across: 0.0,
            pane_scroll: 0.0,
            picking: false,
            narrow: Cell::new(false),
            check: crate::native_tables_check::CheckView::default(),
            columns: native_tables_columns::ColumnsView::default(),
            filters: native_tables_filters::FiltersView::default(),
            pane_keys: std::cell::RefCell::new(Vec::new()),
            rail_scroll: 0.0,
            rail_limit: Cell::new(0.0),
            rail_room: Cell::new(0.0),
            rail_spans: std::cell::RefCell::new(Vec::new()),
            limit: Cell::new(0.0),
            room: Cell::new(0.0),
            pane_limit: Cell::new(0.0),
            pane_room: Cell::new(0.0),
            grid_room: Cell::new(0.0),
            spans: std::cell::RefCell::new(Vec::new()),
            across_limit: Cell::new(0.0),
            order: std::cell::RefCell::new(Order::default()),
            widths: BTreeMap::new(),
            drawn: std::cell::RefCell::new(BTreeMap::new()),
            resizing: None,
            anchor: None,
            sort: BTreeMap::new(),
            history: history::Drawer::default(),
            life: BTreeMap::new(),
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
/// A record's As of, in the history drawer: the operation's digest follows.
const AS_OF: &str = "tables/history/asof/";
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
/// Under 900 px of width, the one control that stands for the rail: it lists the tables in the
/// grid's place, and a press on one shows it.
pub(crate) const PICKER: &str = "tables/picker";

/// Whether `key` is one of the pane's Keep or Undo buttons, which the arrows move among.
fn pane_action(key: &str) -> bool {
    key.starts_with(KEEP) || key.starts_with(UNDO) || key.starts_with(rows::PANE)
}

impl TablesViewState {
    /// Scroll an `act` on a cell, a header or its edge outside the grid's room into view; whether
    /// anything moved.
    pub(crate) fn reveal_listed_control(&mut self, key: &UiKey) -> bool {
        if key.as_str().starts_with(history::KEY) {
            return self.history.reveal(key.as_str());
        }
        if let Some(moved) = self.reveal_columns_control(key.as_str()) {
            return moved;
        }
        if let Some(moved) = self.reveal_filters_control(key.as_str()) {
            return moved;
        }
        if let Some(name) = key.as_str().strip_prefix(SHEET) {
            let rail = self.rail_scroll;
            self.rail_reveal(name.split('/').next().unwrap_or(name));
            return rail != self.rail_scroll;
        }
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
        let (scroll, across) = (self.scroll.get(), self.across);
        self.keep_in_view(at);
        let column = column.split('/').next().unwrap_or(column).to_owned();
        self.reveal(&column);
        scroll != self.scroll.get() || across != self.across
    }

    /// Scroll the rail so table `name`'s line (or Check's, [`crate::native_tables_check::OPEN`])
    /// stands whole in its room.
    pub(crate) fn rail_reveal(&mut self, name: &str) {
        let spans = self.rail_spans.borrow();
        let Some(&(_, top, bottom)) = spans.iter().find(|(n, ..)| n == name) else {
            return;
        };
        let room = self.rail_room.get();
        let scroll = if top < self.rail_scroll {
            top
        } else if bottom > self.rail_scroll + room {
            bottom - room
        } else {
            self.rail_scroll
        };
        drop(spans);
        self.rail_scroll = scroll.clamp(0.0, self.rail_limit.get());
    }

    pub(crate) fn focus(&mut self, key: &str) {
        self.common.last_focus = Some(UiKey::new(key.to_owned()));
    }

    /// The rows the lifecycle filter, the find field and the filters on the columns' values
    /// leave, a row with a staged cell and a new row among them, in the order the sheet's sorted
    /// column gives ([`Self::sort`]); in the sheet's own order where none is sorted. Only this
    /// view is ordered: the sheet and what a Write is compared against are not.
    ///
    /// A cell stands by the value the registry holds, never by words staged in it, and the order
    /// is worked out once for each read, find, sort and set of filters ([`Order`]): typing into
    /// the sorted column, or a filtered one, leaves every row where it is, so Return and Tab reach
    /// the cell that was beside the edited one. A staged cell takes its place once it is written
    /// and read back.
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

    /// Work [`Self::order`] out where the read, the find, the sort, the filter, the rows a
    /// staged change shows or the filters on the columns' values it was worked out from are no
    /// longer the ones in force.
    fn put_in_order(&self, app: &TablesApp, sheet: &Sheet) {
        let find = self.find.value().trim().to_lowercase();
        let filter = self.filter(sheet);
        let of: OrderOf = (
            sheet.name,
            app.reads,
            find,
            self.sort.get(sheet.name).cloned(),
            filter,
            // The rows the filter leaves out that a staged change shows (a retire, an end or a
            // restore pressed, a cell typed): each leaves once its last change is taken back,
            // and the fills stop counting it ([`Self::life_shows`]). A row the filter admits
            // shows whatever is staged in it, so a typed cell moves no row.
            rows::shown_by_staging(app, sheet, filter),
            // The filters judge what the read holds: a staged cell moves no row.
            self.filters_of(sheet),
        );
        if self.order.borrow().of.as_ref() == Some(&of) {
            return;
        }
        let find = of.2.as_str();
        let filters = &of.6;
        let mut rows: Vec<usize> = sheet
            .rows
            .iter()
            .enumerate()
            // A row the lifecycle filter leaves out still shows while a change to it is staged.
            .filter(|(_, row)| self.life_shows(app, sheet, row))
            .filter(|(_, row)| found(app, sheet, row, find))
            // A row with a staged cell, and a new row, show whatever the filters: no row leaves
            // while it is typed in.
            .filter(|(_, row)| !native_tables_filters::leave_out(app, sheet, filters, row))
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
        // A new read of the same sheet, find, sort and filters that moved the selected row: it
        // comes back into view, as a move of the selection would bring it.
        let read_moved = order.of.as_ref().is_some_and(|was| {
            (was.0, &was.2, &was.3, &was.6) == (of.0, &of.2, &of.3, &of.6) && was.1 != of.1
        });
        if read_moved && let Some(pk) = &self.row {
            let was = order.keys.iter().position(|key| key == pk);
            let now = rows.iter().position(|at| sheet.rows[*at].pk == *pk);
            if let (Some(was), Some(now)) = (was, now)
                && was != now
            {
                self.keep_in_view(now);
            }
        }
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
            None => self.scroll.set(0.0),
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

    /// Where the keys go once the rail's line `key` (a table, or Check) is chosen: on it where
    /// the rail stands beside the grid; under 900 px, on the picker, since the list it was
    /// chosen from closes, and keys left on a control not drawn reach the page's default button.
    pub(crate) fn rest_choice(&mut self, key: &str) {
        self.focus(if self.narrow.get() { PICKER } else { key });
    }

    /// Scroll so the grid's row `at` shows whole.
    fn keep_in_view(&self, at: usize) {
        let line = row_height();
        let top = at as f32 * line;
        let room = self.room.get();
        let scroll = self.scroll.get();
        if top < scroll {
            self.scroll.set(top);
        } else if room > 0.0 && top + line > scroll + room {
            self.scroll.set(top + line - room);
        }
    }

    /// Whether the selected cell is in the sheet's last column.
    fn at_last_column(&self, app: &TablesApp) -> bool {
        self.stands(app)
            .is_some_and(|(_, column, _, columns)| column + 1 == columns)
    }

    /// Put the keys on the pane's first Keep or Undo, where it shows one; whether it did.
    fn enter_pane(&mut self) -> bool {
        let Some(first) = self.pane_keys.borrow().first().cloned() else {
            return false;
        };
        self.pane_focus(&first);
        true
    }

    /// Put the keys on one of the pane's Keep or Undo buttons, its list scrolled to show it whole.
    fn pane_focus(&mut self, (key, top, bottom): &(String, f32, f32)) {
        let room = self.pane_room.get();
        if *top < self.pane_scroll {
            self.pane_scroll = *top;
        } else if room > 0.0 && *bottom > self.pane_scroll + room {
            self.pane_scroll = bottom - room;
        }
        self.pane_scroll = self.pane_scroll.clamp(0.0, self.pane_limit.get());
        self.focus(key);
        self.common.focus_visible = true;
    }

    /// Return the keys from the pane to the selected cell.
    fn leave_pane(&mut self) {
        if let (Some(pk), Some(column)) = (self.row.clone(), self.column.clone()) {
            self.focus(&cell_key(&pk, &column));
        }
    }

    /// Up (`-1`) or Down (`1`) from the pane's focused Keep or Undo: the one above or below it;
    /// above the first, the selected cell.
    fn step_pane(&mut self, by: i32) {
        let keys = self.pane_keys.borrow().clone();
        let at = self
            .common
            .last_focus
            .as_ref()
            .and_then(|focus| keys.iter().position(|(key, ..)| key == focus.as_str()));
        let Some(at) = at else {
            self.leave_pane();
            return;
        };
        let to = at as i64 + i64::from(by);
        if to < 0 {
            self.leave_pane();
            return;
        }
        let to = (to as usize).min(keys.len() - 1);
        self.pane_focus(&keys[to]);
    }

    /// Choose a collection and its controls just as a press on its rail entry would.
    fn choose_sheet(&mut self, app: &mut TablesApp, name: &'static str) {
        app.prioritize(name);
        self.missing_record = None;
        self.pending_row = None;
        self.picking = false;
        if self.check.open {
            self.check.open = false;
            app.check(false);
        }
        if self.sheet != name {
            self.sheet = name;
            self.anchor = None;
            self.row = None;
            self.column = None;
            self.scroll.set(0.0);
            self.across = 0.0;
            self.pane_scroll = 0.0;
            self.history.another_table();
        }
        self.rail_reveal(name);
        self.rest_choice(&format!("{SHEET}{name}"));
    }

    /// The selected persisted row, including a record requested while its read is in flight.
    pub(crate) fn row_route(&self) -> Option<String> {
        let (sheet, key) = match &self.pending_row {
            Some(next) => (next.sheet, next.key.as_str()),
            None => (self.sheet, self.row.as_deref()?),
        };
        (!sheet.is_empty() && !rows::is_new(key)).then(|| format!("{sheet}/{key}"))
    }

    /// Open a stored row at the current head, preserving other staged edits. A historical
    /// view waits for Now before selecting the row, since it may have been born since.
    pub(crate) fn open_record(
        &mut self,
        app: &mut TablesApp,
        sheet: &'static str,
        key: &str,
    ) -> Result<(), String> {
        if key.is_empty() || rows::is_new(key) || key.chars().any(char::is_control) {
            return Err("The record has no stored row key".into());
        }
        let moving = app.moving > 0 || app.as_of().is_some();
        if let Some(why) = &app.unread {
            return Err(why.clone());
        }
        let present = app
            .sheet(sheet)
            .is_some_and(|held| held.rows.iter().any(|row| row.pk == key));
        // A hidden Tables tab may have read before the product. Even with unrelated edits
        // staged, it can refresh at its current head before declaring a record absent.
        let refresh_head = moving || (!present && app.snapshot.is_some());
        if app.as_of().is_some() && !app.movable() {
            return Err("Write or discard the changes not written before returning to Now".into());
        }

        // Leaving the current control stages its field and dismisses its menus.
        app.act(self, AppEvent::FocusChanged(None));
        self.choose_sheet(app, sheet);
        self.history.choosing = None;
        self.opening_selection |= app.snapshot.is_none();
        if refresh_head {
            app.ask_point(None, moving)?;
        }
        if !refresh_head && present {
            self.show_row(app, sheet, key, None);
        } else {
            self.row = None;
            self.column = None;
            self.anchor = None;
            self.opening_selection = true;
            self.pending_row = Some(PendingRow {
                sheet,
                key: key.to_owned(),
                column: None,
                current_record: true,
            });
        }
        Ok(())
    }

    /// Show row `pk` of sheet `name` selected, at `column` where the sheet shows that column and
    /// else at its key column: the table chosen as the rail chooses it, and each of the find, the
    /// lifecycle filter and the filters on the columns' values giving way only where it leaves
    /// the row out (the find emptied, the lifecycle filter set to all rows, the filters cleared
    /// by [`Self::clear_filters_for`]'s rule, which the find and the lifecycle filter have given
    /// way to first), so a narrowing that passes the row stands; the row scrolled into view.
    pub(crate) fn show_row(
        &mut self,
        app: &TablesApp,
        name: &'static str,
        pk: &str,
        column: Option<&str>,
    ) {
        self.pending_row = None;
        self.picking = false;
        let moved = self.sheet != name;
        if moved {
            self.sheet = name;
            self.anchor = None;
            self.scroll.set(0.0);
        }
        let Some(sheet) = app.sheet(name) else {
            if app.snapshot.is_none() && app.unread.is_none() {
                app.prioritize(name);
                self.pending_row = Some(PendingRow {
                    sheet: name,
                    key: pk.to_owned(),
                    column: column.map(str::to_owned),
                    current_record: false,
                });
                self.row = None;
                self.column = None;
                self.across = 0.0;
                self.rail_reveal(name);
                self.rest_choice(&format!("{SHEET}{name}"));
            }
            return;
        };
        let column = column
            .filter(|column| sheet.columns.iter().any(|c| c.name == *column))
            .unwrap_or(&sheet.key)
            .to_owned();
        if let Some(row) = sheet.rows.iter().find(|row| row.pk == pk) {
            if !found(app, sheet, row, &self.find.value().trim().to_lowercase()) {
                self.find = TextInputState::new(String::new());
            }
            // A row the lifecycle filter leaves out (a retired one a finding names) shows among
            // all.
            if !self.life_shows(app, sheet, row) {
                self.life.insert(name, rows::Filter::All);
            }
        }
        // Last, as the rule clears the filters only where nothing else leaves the row out: a
        // row they leave out shows once they are cleared; one they pass keeps them.
        self.clear_filters_for(app, sheet, pk);
        self.select(pk, &column);
        if moved {
            // The columns' places are the last sheet's until this one is laid out.
            self.across = 0.0;
        }
        if let Some(at) = self
            .shown_rows(app, sheet)
            .iter()
            .position(|row| row.pk == pk)
        {
            self.keep_in_view(at);
        }
        self.pane_scroll = 0.0;
        self.rail_reveal(name);
    }

    /// Follow a pending reference as its sheet arrives. A failed read or omitted family
    /// leaves the keys on the rail instead of a cell that has disappeared.
    pub(crate) fn settle_opening(&mut self, app: &TablesApp) {
        if !self.opening_selection {
            return;
        }
        if self
            .pending_row
            .as_ref()
            .is_some_and(|next| next.current_record)
            && app.asked > 0
        {
            return;
        }
        let finished = app.snapshot.is_some() || app.unread.is_some();
        if finished {
            self.opening_selection = false;
        }
        if self
            .pending_row
            .as_ref()
            .is_some_and(|next| finished || app.sheet(next.sheet).is_some())
            && let Some(next) = self.pending_row.take()
            && self.sheet == next.sheet
        {
            let sheet = app.sheet(next.sheet);
            if next.current_record && (app.as_of().is_some() || app.move_failed) {
                self.missing_record = Some(
                    app.note
                        .clone()
                        .unwrap_or_else(|| "Could not read the current table".into()),
                );
            } else if sheet.is_some_and(|s| s.rows.iter().any(|r| r.pk == next.key)) {
                self.show_row(app, next.sheet, &next.key, next.column.as_deref());
            } else if next.current_record && app.unread.is_none() {
                self.missing_record = Some(match sheet {
                    Some(s) => format!("Row {} is absent from {}", next.key, s.title),
                    None => format!("Table {} is unavailable", next.sheet),
                });
            }
        }
        if finished && app.sheet(self.sheet).is_none() && !self.sheet.is_empty() {
            self.row = None;
            self.column = None;
            self.anchor = None;
            self.history.another_table();
            self.sheet = app.current(self).unwrap_or("");
            if !self.picking {
                self.rest_choice(&format!("{SHEET}{}", self.sheet));
            }
        }
    }

    /// Close the open cell: stage what it holds unless `cancel` or it is what it opened with.
    /// An enum's list stages its marked word however the cell closes (Return, Tab, a press
    /// elsewhere), as the field completes what was typed to it.
    pub(crate) fn close_edit(&mut self, app: &mut TablesApp, cancel: bool) {
        let Some(editing) = self.editing.take() else {
            return;
        };
        let text = editing
            .choice
            .as_ref()
            .and_then(Choice::marked)
            .map_or_else(|| editing.input.value().to_owned(), str::to_owned);
        if !cancel && text != editing.seed {
            app.stage_over(self.sheet, &editing.pk, &editing.column, text, editing.saw);
        }
        self.focus(&cell_key(&editing.pk, &editing.column));
    }

    fn open_edit(&mut self, app: &mut TablesApp, pk: &str, column: &str) {
        let Some(sheet) = app.sheet(self.sheet) else {
            return;
        };
        let shown = sheet
            .rows
            .iter()
            .find(|r| r.pk == pk)
            .and_then(|r| r.cells.get(column));
        if !app.takes(self.sheet, pk, column) || app.fixed() {
            return;
        }
        let Some(compiled) = sheet.columns.iter().find(|c| c.name == column) else {
            return;
        };
        let at = (self.sheet.to_owned(), pk.to_owned(), column.to_owned());
        let held = app.staged.get(&at).cloned().unwrap_or_else(|| {
            if shown.is_some_and(|c| !c.values.is_empty()) {
                String::new()
            } else {
                shown.map(|c| c.value().to_owned()).unwrap_or_default()
            }
        });
        let opens = crate::native_tables_choices::opens(compiled);
        if opens == Opens::Toggle {
            // A bool is switched, not typed: its other value is staged at once. A conflicted
            // one switches to nothing the person saw: its values are kept from the pane.
            if !app.staged.contains_key(&at) && shown.is_some_and(|c| !c.values.is_empty()) {
                return;
            }
            let saw = app.held(self.sheet, pk, column);
            let sheet = self.sheet;
            let other = crate::native_tables_choices::toggled(&held);
            app.stage_over(sheet, pk, column, other, saw);
            return;
        }
        let seed = held;
        let choice = compiled
            .compiled
            .filter(|_| opens == Opens::List)
            .and_then(|c| match c.ty {
                // A reference opens on the rows of its target, read by their words.
                aterm_db::typed::Ty::Ref(to) => {
                    rows::choice(app, compiled.links.as_deref().unwrap_or(to), c.null, &seed)
                }
                _ => Choice::of(c, &seed),
            });
        // The field shows the marked choice as it reads: a row by its words, not its key.
        let shows = choice
            .as_ref()
            .and_then(Choice::marked_said)
            .map_or_else(|| seed.clone(), str::to_owned);
        let mut input = TextInputState::new(shows);
        input.select_all();
        self.anchor = None;
        self.editing = Some(Editing {
            pk: pk.to_owned(),
            column: column.to_owned(),
            seed,
            saw: app.held(self.sheet, pk, column),
            input,
            choice,
        });
        self.focus(EDIT);
    }

    /// Move the open list's mark `by` words; in an enum's list the field takes the word marked.
    fn step_choice(&mut self, by: i32) {
        if let Some(editing) = self.editing.as_mut()
            && let Some(choice) = editing.choice.as_mut()
        {
            choice.step(by);
            if let Some(word) = choice.marked_said() {
                editing.input = TextInputState::new(word.to_owned());
                editing.input.select_all();
            }
        }
    }

    /// The open list's word `n` chosen, by a press or by Return: an enum's cell stages it and
    /// closes, a set's puts it in or takes it out and stays open. Whether a list was open.
    fn choose(&mut self, app: &mut TablesApp, n: usize) -> bool {
        let Some(editing) = self.editing.as_mut() else {
            return false;
        };
        let Some(choice) = editing.choice.as_mut() else {
            return false;
        };
        let Some(word) = choice.said.get(n).cloned() else {
            return true;
        };
        choice.at = Some(n);
        if choice.many {
            let text = choice.toggled(editing.input.value(), n);
            choice.wrote.clone_from(&text);
            editing.input = TextInputState::new(text);
            self.focus(EDIT);
        } else {
            editing.input = TextInputState::new(word);
            self.close_edit(app, false);
        }
        true
    }

    fn select(&mut self, pk: &str, column: &str) {
        self.pending_row = None;
        self.missing_record = None;
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
                    set(self, content_width(app, found, sheet, sheet.rows.iter()));
                } else if self.resizing.is_some() || dx.abs() >= 1.0 {
                    set(self, from + dx);
                }
                self.resizing = (!*done).then(|| (column.to_owned(), from));
                if *done {
                    self.rest_focus(&format!("{EDGE}{column}"), false);
                }
            }
            None => set(self, content_width(app, found, sheet, sheet.rows.iter())),
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
        let names: Vec<&str> = self
            .shown_columns(app, sheet)
            .iter()
            .map(|c| c.name.as_str())
            .collect();
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
        let columns = self.shown_columns(app, sheet);
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
                self.shown_columns(app, sheet)
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

/// Whether a change to `row` of `sheet` is staged.
fn staged(app: &TablesApp, sheet: &Sheet, row: &SheetRow) -> bool {
    app.staged
        .keys()
        .any(|(c, p, _)| c == sheet.name && *p == row.pk)
}

/// Whether the find's words `find` (trimmed and lowered) leave `row` of `sheet`: none are typed,
/// the row's key starts with them, a change to it is staged, or a cell of it but a linked count
/// holds them.
fn found(app: &TablesApp, sheet: &Sheet, row: &SheetRow, find: &str) -> bool {
    find.is_empty()
        || row.pk.starts_with(find)
        || staged(app, sheet, row)
        || sheet.columns.iter().any(|column| {
            column.group != Group::Linked
                && row
                    .cells
                    .get(&column.name)
                    .is_some_and(|c| c.text.to_lowercase().contains(find))
        })
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
    event.edit(input);
}

impl TablesApp {
    /// The page's reducer, apart from the runtime's bookkeeping.
    fn act(&mut self, view: &mut TablesViewState, event: AppEvent) -> bool {
        // A person's next command takes precedence over an external row reveal still waiting
        // for its data. In particular Check, Find and the columns menu each own their focus.
        if matches!(event, AppEvent::Action(_))
            && view
                .pending_row
                .as_ref()
                .is_some_and(|row| row.current_record)
        {
            view.pending_row = None;
        }
        if let Some(done) = native_tables_filters::act(self, view, &event) {
            return done;
        }
        if let Some(done) = native_tables_columns::act(self, view, &event) {
            return done;
        }
        match event {
            #[cfg(any(a11y_tree, test))]
            AppEvent::RevealControl(key) => view.reveal_listed_control(&key),
            // Text an assistive technology inserts goes where typed text would.
            #[cfg(any(a11y_tree, test))]
            AppEvent::InsertText(text) => {
                self.act(view, AppEvent::TextInput(TextInputEvent::Commit(text)))
            }
            AppEvent::FocusChanged(focus) => {
                // A press on a word of the open cell's list is the cell's, and leaves it open.
                if view.editing.is_some()
                    && focus
                        .as_ref()
                        .is_none_or(|k| k.as_str() != EDIT && !k.as_str().starts_with(CHOICE))
                {
                    // Leaving an open cell keeps what was typed in it.
                    view.close_edit(self, false);
                }
                // A cell the keyboard or an assistive technology moved to is the selection.
                if let Some((pk, column)) = focus
                    .as_ref()
                    .and_then(|k| k.as_str().strip_prefix(CELL))
                    .and_then(|rest| rest.split_once('/'))
                {
                    view.pending_row = None;
                    view.missing_record = None;
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
                            if let Some(choice) = editing.choice.as_mut() {
                                choice.follow(value);
                            }
                        }
                        return true;
                    }
                    if key == FIND {
                        view.find = TextInputState::new(value.clone());
                        view.scroll.set(0.0);
                        return true;
                    }
                    if key == history::FIND_RECORDS {
                        view.history.find = TextInputState::new(value.clone());
                        self.history_found(view);
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
                        history::FIND_RECORDS => Some(&mut view.history.find),
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
                if key == EDIT || key == FIND || key == history::FIND_RECORDS {
                    view.focus(&key);
                    return true;
                }
                if let Some(done) = crate::native_tables_check::act(self, view, &key) {
                    return done;
                }
                if let Some(done) = claims::act(self, view, &key) {
                    return done;
                }
                if let Some(done) = rows::act(self, view, &key) {
                    return done;
                }
                if key == PICKER {
                    view.close_edit(self, false);
                    view.pending_row = None;
                    view.picking = !view.picking;
                    view.focus(PICKER);
                    return true;
                }
                if let Some(taken) = self.history_act(view, &key) {
                    return taken;
                }
                if let Some(name) = key.strip_prefix(SHEET) {
                    view.close_edit(self, false);
                    if let Some(name) = self
                        .listed()
                        .iter()
                        .map(|listed| listed.name)
                        .find(|listed| *listed == name)
                    {
                        view.choose_sheet(self, name);
                    }
                    return true;
                }
                if let Some(n) = key.strip_prefix(CHOICE) {
                    return n.parse::<usize>().is_ok_and(|n| view.choose(self, n));
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
                        self.unstage(&(view.sheet.to_owned(), pk.clone(), column.to_owned()));
                        // Its Undo is gone: the keys return to the selected cell.
                        if view
                            .common
                            .last_focus
                            .as_ref()
                            .is_some_and(|k| k.as_str() == key)
                        {
                            let at = view.column.clone().unwrap_or_else(|| column.to_owned());
                            view.focus(&cell_key(&pk, &at));
                        }
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
                    let operation = self
                        .snapshot
                        .as_ref()
                        .and_then(|s| {
                            s.sheets
                                .iter()
                                .flat_map(|sheet| &sheet.written)
                                .map(|w| w.operation)
                                .find(|op| aterm_codec::hex::encode_lower(op) == hex)
                        })
                        .or_else(|| {
                            self.registry
                                .records
                                .iter()
                                .map(|record| record.operation)
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
                        self.born.clear();
                        self.show_born();
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
                    .map(|k| k.as_str().to_owned())
                    // A word of the open cell's list that holds the keys (an assistive
                    // technology's focus, a press) keys as the open field does.
                    .map(|k| {
                        if k.starts_with(CHOICE) && view.editing.is_some() {
                            EDIT.to_owned()
                        } else {
                            k
                        }
                    });
                match focus.as_deref() {
                    Some(EDIT) if view.editing.is_some() => {
                        let editing = view.editing.as_ref();
                        let marked = editing.and_then(|e| e.choice.as_ref()).and_then(|c| c.at);
                        let spaces = editing.is_some_and(|e| {
                            e.choice.as_ref().is_some_and(|c| c.spaces(e.input.value()))
                        });
                        match event {
                            // Enter keeps what was typed (in an enum's list, the word marked)
                            // and moves to the cell below, as a spreadsheet's does; Escape
                            // leaves the cell as it was.
                            TextInputEvent::Submit => {
                                view.close_edit(self, false);
                                view.step(self, 1, 0);
                                true
                            }
                            TextInputEvent::Cancel => {
                                view.close_edit(self, true);
                                true
                            }
                            // Space in a set's list puts the marked word in or takes it out,
                            // while the field holds what the list wrote; else it is typed.
                            TextInputEvent::Commit(text) if spaces && text == " " => {
                                if let Some(n) = marked {
                                    view.choose(self, n);
                                }
                                true
                            }
                            event => {
                                if let Some(editing) = view.editing.as_mut() {
                                    edit(&mut editing.input, event);
                                    let typed = editing.input.value().to_owned();
                                    if let Some(choice) = editing.choice.as_mut() {
                                        choice.follow(&typed);
                                    }
                                }
                                true
                            }
                        }
                    }
                    Some(FIND) => {
                        if matches!(event, TextInputEvent::Cancel) {
                            view.find = TextInputState::new(String::new());
                        } else if !matches!(event, TextInputEvent::Submit) {
                            edit(&mut view.find, event);
                        }
                        view.scroll.set(0.0);
                        true
                    }
                    // History's Find, as the grid's: Escape empties it.
                    Some(history::FIND_RECORDS) => {
                        if matches!(event, TextInputEvent::Cancel) {
                            view.history.find = TextInputState::new(String::new());
                        } else if !matches!(event, TextInputEvent::Submit) {
                            edit(&mut view.history.find, event);
                        }
                        self.history_found(view);
                        true
                    }
                    // Left from a pane's Keep or Undo returns to the selected cell.
                    Some(key) if pane_action(key) => match event {
                        TextInputEvent::Left { .. } => {
                            view.leave_pane();
                            true
                        }
                        TextInputEvent::Right { .. } => true,
                        _ => false,
                    },
                    // Right past the sheet's last column goes on to the pane's first Keep or
                    // Undo, where the selected row has one.
                    Some(key)
                        if key.starts_with(CELL)
                            && matches!(event, TextInputEvent::Right { extend: false })
                            && view.at_last_column(self)
                            && view.enter_pane() =>
                    {
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
                        // Typing on a selected cell opens it with what was typed; on a bool's,
                        // Space switches it, as Return does. Space on a cell that opens a list
                        // opens it on what the cell holds; a typed word marks its word there (in
                        // a set's list, over the members the cell holds).
                        TextInputEvent::Commit(text) if !text.is_empty() => {
                            if let (Some(pk), Some(column)) =
                                (view.row.clone(), view.column.clone())
                            {
                                let opens = self
                                    .sheet(view.sheet)
                                    .and_then(|s| s.columns.iter().find(|c| c.name == column))
                                    .map(crate::native_tables_choices::opens);
                                if opens == Some(Opens::Toggle) && text != " " {
                                    return true;
                                }
                                view.open_edit(self, &pk, &column);
                                if let Some(editing) = view.editing.as_mut() {
                                    match editing.choice.as_mut() {
                                        _ if opens == Some(Opens::List) && text == " " => {}
                                        Some(choice) if choice.many => choice.follow(&text),
                                        choice => {
                                            editing.input = TextInputState::new(text.clone());
                                            if let Some(choice) = choice {
                                                choice.follow(&text);
                                            }
                                        }
                                    }
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
            // Up and Down in an open cell's list move its mark; in an enum's, the field takes the
            // word marked, which Return stages.
            AppEvent::StepFocus(rows)
                if view
                    .editing
                    .as_ref()
                    .is_some_and(|editing| editing.choice.is_some()) =>
            {
                view.step_choice(rows);
                true
            }
            // Up and Down on a pane's Keep or Undo move among them; Up from the first returns to
            // the selected cell.
            AppEvent::StepFocus(rows)
                if view
                    .common
                    .last_focus
                    .as_ref()
                    .is_some_and(|key| pane_action(key.as_str())) =>
            {
                view.step_pane(rows);
                true
            }
            // Up and Down in the history drawer's list move from line to line, which comes
            // into view; the grid's selection stays.
            AppEvent::StepFocus(rows)
                if view
                    .common
                    .last_focus
                    .as_ref()
                    .is_some_and(|key| view.history.step(key.as_str(), rows).is_some()) =>
            {
                let to = view
                    .common
                    .last_focus
                    .as_ref()
                    .and_then(|key| view.history.step(key.as_str(), rows));
                if let Some(to) = to {
                    view.history.reveal(&to);
                    view.focus(&to);
                }
                true
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
            // The wheel over an open cell's list moves its mark, as Up and Down do.
            AppEvent::ScrollAt { lines, key }
                if key.as_str().starts_with("tables/choice")
                    && view
                        .editing
                        .as_ref()
                        .is_some_and(|editing| editing.choice.is_some()) =>
            {
                view.step_choice(lines.signum());
                true
            }
            // A wheel over the open history drawer scrolls its list, not the grid's rows; over
            // the closed one, which has no list, the grid's.
            AppEvent::ScrollAt { lines, key }
                if key.as_str().starts_with(history::KEY) && view.history.open =>
            {
                view.history.wheel(lines);
                true
            }
            AppEvent::ScrollAt { lines, key }
                if key.as_str().starts_with("tables/rail")
                    || key.as_str().starts_with(SHEET)
                    || key.as_str().starts_with("tables/view") =>
            {
                view.rail_scroll = (view.rail_scroll + lines as f32 * row_height())
                    .clamp(0.0, view.rail_limit.get());
                true
            }
            AppEvent::ScrollAt { lines, key } if key.as_str().starts_with("tables/pane") => {
                view.pane_scroll = (view.pane_scroll + lines as f32 * row_height())
                    .clamp(0.0, view.pane_limit.get());
                true
            }
            AppEvent::ScrollLines(lines) | AppEvent::ScrollAt { lines, .. }
                if view.check.open && !view.picking =>
            {
                view.check.scroll = (view.check.scroll + lines as f32 * row_height())
                    .clamp(0.0, view.check.limit.get());
                true
            }
            AppEvent::ScrollLines(lines) | AppEvent::ScrollAt { lines, .. } => {
                view.scroll.set(
                    (view.scroll.get() + lines as f32 * row_height()).clamp(0.0, view.limit.get()),
                );
                true
            }
            AppEvent::ScrollAcross(lines) => {
                view.across = (view.across + lines as f32 * ACROSS_STEP * scale().max(1.0))
                    .clamp(0.0, view.across_limit.get());
                true
            }
            AppEvent::ScrollPage(pages) => {
                view.scroll.set(
                    (view.scroll.get() + pages as f32 * view.room.get().max(row_height()))
                        .clamp(0.0, view.limit.get()),
                );
                true
            }
            _ => false,
        }
    }

    /// Whether Tab (or Shift-Tab) leaves an open list of a column's values: the keys are on it.
    /// The list is one stop of Tab's: the page closes it, the keys back on the control it opened
    /// from ([`AppEvent::StepAcross`]), and Tab goes on from there.
    pub(crate) fn tab_leaves_list(&self, view: &TablesViewState) -> bool {
        view.filters_keyed()
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
        let names = view.shown_columns(self, sheet);
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
                                .map(|cell| cell.value().to_owned())
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
            if self.takes(sheet, &pk, &column) {
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
                view.scroll.set(0.0);
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
        let names = view.shown_columns(self, sheet);
        let lands = |row: usize, column: usize| {
            let (row, column) = (shown.get(row)?, names.get(column)?);
            Some(Landing {
                pk: row.pk.clone(),
                column: column.name.clone(),
                takes: self.takes(sheet.name, &row.pk, &column.name),
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
            history::FIND_RECORDS => Some(&view.history.find),
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
        if view.pending_row.is_none()
            && let Some(shown) = self.current(view)
        {
            view.sheet = shown;
        }
        let open = (view.columns.open, view.filters.is_open());
        let acted = self.act(view, event);
        if self.snapshot.is_none() {
            view.opening_selection = true;
        }
        // The selection moves off a column no longer drawn, first, since the drawer's Cell scope
        // lists the selected cell's lines; then the keys off a line of a column's values the
        // list no longer offers, and last off a drawer line the next layout does not lay out.
        // Each of the two moves only keys on its own controls and rests them on none of the
        // other's (a line of the list, its mark or chip, or the find; Hide history), so neither
        // strands keys the other moved. A list of columns or of a column's values the event
        // closed repaints, whatever the page made of the event itself.
        let moved = view.settle_columns(self);
        let filtered = view.settle_filters(self);
        let rested = view.settle_history_keys(self);
        let settled =
            moved || filtered || rested || open != (view.columns.open, view.filters.is_open());
        self.recheck();
        if acted || settled {
            view.common.presentation_revision = view.common.presentation_revision.wrapping_add(1);
            cx.repaint(DamageRegion::All);
        }
        if !acted {
            return EventResult::Bubble;
        }
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

pub(crate) fn scale() -> f32 {
    crate::native_appearance::text_scale()
}

pub(crate) fn row_height() -> f32 {
    28.0 * scale().max(1.0)
}

/// How tall the grid's group line and its type line stand, over and under its header line.
fn head_line() -> f32 {
    22.0 * scale().max(1.0)
}

/// What the grid keeps over and under its rows ([`grid`]): its rules (three points), the
/// group line, the header line and the type line. A grid `height` tall leaves its rows that
/// height less this; [`page`] weighs the cell bar on it.
fn grid_chrome() -> f32 {
    3.0 + 2.0 * head_line() + row_height()
}

pub(crate) fn group(key: impl Into<std::sync::Arc<str>>) -> UiNode {
    UiNode::new(
        key,
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
    )
}

pub(crate) fn styled(key: impl Into<std::sync::Arc<str>>, style: StyleRef) -> UiNode {
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
pub(crate) fn fitted(
    key: impl Into<std::sync::Arc<str>>,
    text: &str,
    style: StyleRef,
    width: f32,
) -> UiNode {
    words(
        key,
        one_line(text, SemanticRole::Text, style, width),
        style,
        Length::Fixed(width.max(0.0)),
    )
}

pub(crate) fn button(
    key: &str,
    label: &str,
    spoken: &str,
    style: StyleRef,
    enabled: bool,
) -> UiNode {
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
pub(crate) fn press(key: &str, spoken: String) -> UiNode {
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

/// `n rows`, `1 row`, `n of m rows`.
fn rows_words(shown: usize, all: usize) -> String {
    let rows = if all == 1 { "row" } else { "rows" };
    if shown == all {
        format!("{all} {rows}")
    } else {
        format!("{shown} of {all} {rows}")
    }
}

/// When an operation's writer says it wrote: `just now`, or `Sat 7 Nov 19:12` on this Mac's clock.
/// A time ahead of this Mac's clock is `just now` here; [`ago_words`] and [`on_words`] say such
/// a claim as its date.
pub(crate) fn when_words(at_ms: u64, now_ms: u64) -> String {
    if now_ms.saturating_sub(at_ms) < 60_000 {
        return "just now".into();
    }
    date_words(at_ms)
}

/// `Sat 7 Nov 19:12`: `at_ms` on this Mac's clock.
fn date_words(at_ms: u64) -> String {
    let (days, of_day) = local_day(at_ms);
    let (_, month, day) = aterm_types::rfc3339::civil_from_days(days);
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

/// The day `at_ms` falls on this Mac's clock (days since 1970-01-01), and its seconds into it.
fn local_day(at_ms: u64) -> (i64, i64) {
    let unix = i64::try_from(at_ms / 1000).unwrap_or(i64::MAX);
    let local = unix + crate::presence::local_offset_at(unix).unwrap_or(0);
    (local.div_euclid(86_400), local.rem_euclid(86_400))
}

/// `2026-11-14`: the day `days` since 1970-01-01, and `of_day` seconds into it as ` 10:23`.
/// Its words order as the days and times do.
fn day_words(days: i64, of_day: Option<i64>) -> String {
    let (year, month, day) = aterm_types::rfc3339::civil_from_days(days);
    let mut words = format!("{year:04}-{month:02}-{day:02}");
    if let Some(of_day) = of_day {
        words.push_str(&format!(
            " {:02}:{:02}",
            of_day / 3600,
            (of_day % 3600) / 60
        ));
    }
    words
}

/// `2026-11-14 10:23`: a time a Mac stated, on this Mac's clock, in words that order as the
/// times do.
pub(crate) fn stamp_words(at_ms: u64) -> String {
    let (days, of_day) = local_day(at_ms);
    day_words(days, Some(of_day))
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

/// How wide `column` of `sheet` is where it shows its header (its name and its type's words)
/// and each of `rows`' values whole, with the tag a cell wears beside its value (`2 values`,
/// `differs`).
fn content_width<'a>(
    app: &TablesApp,
    column: &SheetColumn,
    sheet: &Sheet,
    rows: impl Iterator<Item = &'a SheetRow>,
) -> f32 {
    // The name keeps room beside it for the column's filter mark; the type's words, under it,
    // need none.
    let header = (text_width(&column.label, SemanticRole::Text, StyleRef::Note)
        + native_tables_filters::HEAD_MARK)
        .max(text_width(
            &crate::native_tables_choices::type_words(column, |name| app.title_of(name)),
            SemanticRole::Text,
            StyleRef::Note,
        ))
        + HEAD_ROOM;
    let widest = rows
        .filter_map(|row| row.cells.get(&column.name).map(|cell| (row, cell)))
        .map(|(row, cell)| {
            let tag = if !cell.values.is_empty() {
                text_width(
                    &format!("{} values", cell.values.len()),
                    SemanticRole::Text,
                    StyleRef::Chip,
                )
            } else if sheet
                .observed
                .get(&(row.pk.clone(), column.name.clone()))
                .is_some_and(|seen| disagrees(Some(cell), seen))
            {
                text_width("differs", SemanticRole::Text, StyleRef::Chip)
            } else {
                -10.0
            };
            // The tag, its padding and the gap before it.
            text_width(&cell.text, SemanticRole::Text, StyleRef::Primary) + tag + 10.0
        })
        .fold(0.0_f32, f32::max);
    // A staged value the save would refuse shows beside its `refused` tag, a label another row
    // claims beside its `in use`.
    let refused = app
        .staged
        .iter()
        .filter(|((name, _, col), _)| name == sheet.name && *col == column.name)
        .filter_map(|(at, text)| {
            let tag = match app.refusal(at) {
                Some(_) => ("refused", StyleRef::Danger),
                None => app
                    .claimed
                    .get(at)
                    .map(|_| (claims::IN_USE, StyleRef::Note))?,
            };
            Some(
                text_width(text, SemanticRole::Text, StyleRef::Primary)
                    + text_width(tag.0, SemanticRole::Text, tag.1)
                    + 10.0,
            )
        })
        .fold(0.0_f32, f32::max);
    header.max(widest).max(refused) + 20.0
}

/// How wide each column draws: as a person sized it, else as its header or the widest of its
/// first rows needs, within bounds.
fn widths(
    app: &TablesApp,
    view: &TablesViewState,
    sheet: &Sheet,
    columns: &[&SheetColumn],
) -> Vec<f32> {
    let sized = view.widths.get(sheet.name);
    columns
        .iter()
        .map(|column| {
            sized
                .and_then(|sized| sized.get(&column.name))
                .copied()
                .unwrap_or_else(|| {
                    content_width(app, column, sheet, sheet.rows.iter().take(200))
                        .clamp(56.0 * scale(), 260.0 * scale())
                })
        })
        .collect()
}

/// The rail's heading for the collections of `face`: the owner's are STORED, what a Mac or a
/// tool sets is SET BY ATERM, and reference rows are REFERENCE.
const fn rail_group(face: aterm_db::typed::Face) -> &'static str {
    use aterm_db::typed::Face;
    match face {
        Face::Owner => "STORED",
        Face::Mac | Face::Tool | Face::Carried => "SET BY ATERM",
        Face::Seeded => "REFERENCE",
    }
}

/// The rail: every collection the last read listed, under the title of its table family where
/// it lists more than one (one family's title says nothing the page does not), and in its
/// family under the heading of its group ([`rail_group`]), each with its rows and the cells of
/// it not yet written, in a list that scrolls where the window is too short for it; when it was
/// read stays at its foot.
#[expect(clippy::too_many_lines, reason = "one rail, laid out in one pass")]
fn rail(app: &TablesApp, view: &TablesViewState, width: f32, height: f32) -> UiNode {
    let line = 26.0 * scale().max(1.0);
    let inner = width - 24.0;
    let current = app.current(view);
    let mut children = Vec::new();
    // How tall the list is so far, and where each table's line stands in it.
    let mut tall = 0.0_f32;
    let mut spans = Vec::new();
    let step = |tall: &mut f32, h: f32| *tall += h + 2.0;
    let mut heading: Option<(&str, &str)> = None;
    // The families in the order the read listed them: the first's group headings keep their
    // own keys, a later family's are keyed under its title.
    let mut families: Vec<&'static str> = Vec::new();
    for listed in app.listed() {
        if !families.contains(&listed.family) {
            families.push(listed.family);
        }
    }
    let titled = families.len() > 1;
    let first = families.first().copied();
    // Each family's tables together, and in it each group's, in the order the read listed them.
    let listed = families.iter().flat_map(|family| {
        ["STORED", "SET BY ATERM", "REFERENCE"]
            .into_iter()
            .flat_map(move |word| {
                app.listed().iter().filter(move |listed| {
                    listed.family == *family && rail_group(listed.face) == word
                })
            })
    });
    for listed in listed {
        let name = listed.name;
        let title = listed.title;
        let group_word = rail_group(listed.face);
        let family_key = if Some(listed.family) == first {
            String::new()
        } else {
            format!("{}/", crate::native_ui::key_word(listed.family))
        };
        if titled && heading.is_none_or(|(family, _)| family != listed.family) {
            if heading.is_some() {
                children.push(
                    group(format!(
                        "tables/rail/gap/family/{}",
                        crate::native_ui::key_word(listed.family)
                    ))
                    .layout(Layout::default().height(Length::Fixed(10.0 * scale()))),
                );
                step(&mut tall, 10.0 * scale());
            }
            children.push(
                fitted(
                    format!(
                        "tables/rail/family/{}",
                        crate::native_ui::key_word(listed.family)
                    ),
                    listed.family,
                    StyleRef::Plain,
                    inner,
                )
                .layout(
                    Layout::default()
                        .width(Length::Fixed(inner))
                        .height(Length::Fixed(22.0 * scale())),
                ),
            );
            step(&mut tall, 22.0 * scale());
            heading = Some((listed.family, ""));
        }
        if heading != Some((listed.family, group_word)) {
            if heading.is_some_and(|(family, word)| family == listed.family && !word.is_empty()) {
                children.push(
                    group(format!(
                        "tables/rail/gap/{family_key}{}",
                        crate::native_ui::key_word(group_word)
                    ))
                    .layout(Layout::default().height(Length::Fixed(6.0 * scale()))),
                );
                step(&mut tall, 6.0 * scale());
            }
            heading = Some((listed.family, group_word));
            children.push(
                fitted(
                    format!(
                        "tables/rail/group/{family_key}{}",
                        crate::native_ui::key_word(group_word)
                    ),
                    group_word,
                    StyleRef::Note,
                    inner,
                )
                .layout(
                    Layout::default()
                        .width(Length::Fixed(inner))
                        .height(Length::Fixed(22.0 * scale())),
                ),
            );
            step(&mut tall, 22.0 * scale());
        }
        let key = format!("{SHEET}{name}");
        let count = app.sheet(name).map(rows::counted);
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
        let chosen = current == Some(name) && !view.check.open;
        let mut painted = vec![fitted(
            format!("{key}/name"),
            title,
            if chosen {
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
        if chosen {
            row.push(styled(format!("{key}/band"), StyleRef::Code));
        }
        row.push(press(
            &key,
            format!(
                "{title}: {}{}",
                count.map_or_else(|| "rows unread".to_owned(), |n| rows_words(n, n)),
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
        spans.push((name.to_owned(), tall, tall + line));
        step(&mut tall, line);
    }
    // Under the tables, the view of the whole registry: Check, a line with no count, which is
    // inside the view.
    if app.snapshot.is_some() {
        children.push(
            group("tables/rail/gap/views")
                .layout(Layout::default().height(Length::Fixed(10.0 * scale()))),
        );
        step(&mut tall, 10.0 * scale());
        let key = crate::native_tables_check::OPEN;
        let chosen = view.check.open;
        let face = group(format!("{key}/face"))
            .layout(
                Layout::row()
                    .padding(Insets::symmetric(10.0, 0.0))
                    .height(Length::Fixed(line)),
            )
            .children(vec![fitted(
                format!("{key}/name"),
                "Check",
                if chosen {
                    StyleRef::Primary
                } else {
                    StyleRef::Plain
                },
                inner - 20.0,
            )])
            .paint_only_tree();
        let mut row = vec![];
        if chosen {
            row.push(styled(format!("{key}/band"), StyleRef::Code));
        }
        row.push(press(
            key,
            "Check: what check finds and the store's state".to_owned(),
        ));
        row.push(face);
        children.push(
            group(format!("{key}/seat"))
                .layout(Layout::default().height(Length::Fixed(line)))
                .children(row),
        );
        spans.push((key.to_owned(), tall, tall + line));
        step(&mut tall, line);
    }
    let foot_h = 20.0 * scale();
    let room = (height
        - 24.0
        - if app.snapshot.is_some() {
            foot_h + 2.0
        } else {
            0.0
        })
    .max(line);
    let limit = (tall - 2.0 - room).max(0.0);
    view.rail_limit.set(limit);
    view.rail_room.set(room);
    *view.rail_spans.borrow_mut() = spans;
    let mut column = vec![
        group("tables/rail/list")
            .layout(
                Layout::column()
                    .gap(2.0)
                    .height(Length::Fixed(room))
                    .clipped()
                    .scrolled(view.rail_scroll.min(limit)),
            )
            .room(room, limit > 0.0)
            .lists_every_row()
            .children(children),
    ];
    if let Some(snapshot) = &app.snapshot {
        column.push(
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
                    .height(Length::Fixed(foot_h)),
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
    .children(column)
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
    // The columns the grid draws, each folded group's stub after its last
    // ([`native_tables_columns`]); a range counts the drawn columns alone.
    let stubs = view.stubs(app, sheet);
    let columns = view.laid_columns(app, sheet, &stubs);
    let drawn_at: Vec<Option<usize>> = columns
        .iter()
        .scan(0_usize, |n, column| {
            Some((!native_tables_columns::is_stub(column)).then(|| {
                *n += 1;
                *n - 1
            }))
        })
        .collect();
    let mut widths = widths(app, view, sheet, &columns);
    let line = row_height();
    let head_line = head_line();
    let rows = view.shown_rows(app, sheet);
    // The frame rules the grid's four sides: a point at each.
    let room_across = (width - 2.0).max(1.0);
    // The row numbers stay at the left, then the key column (the first), at most half of what
    // is left; the rest scroll across in the room beside them.
    // A row added, retired, ended or restored and not yet written says so in its number's seat.
    let row_words: Vec<Option<&'static str>> = rows
        .iter()
        .map(|row| app.row_word(sheet, &row.pk))
        .collect();
    let gutter = (row_words
        .iter()
        .flatten()
        .map(|word| text_width(word, SemanticRole::Text, StyleRef::Unwritten))
        .fold(
            text_width(
                &rows.len().max(1).to_string(),
                SemanticRole::Text,
                StyleRef::Primary,
            ),
            f32::max,
        )
        + 16.0)
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
        view.columns.laid_out(sheet.name);
        *view.drawn.borrow_mut() = columns
            .iter()
            .zip(&widths)
            .map(|(column, w)| (column.name.clone(), *w))
            .collect();
    }
    let across_limit = (beside - room_beside).max(0.0);
    view.across_limit.set(across_limit);
    let across = view.across.min(across_limit);
    // Where each column's filter mark ends, across the grid (its left rule, the row numbers,
    // the key column, then the rest as scrolled, held inside the room beside the key column):
    // its list opens under it ([`native_tables_filters::list`]).
    {
        let inset = native_tables_filters::mark_inset();
        let mut marks = BTreeMap::new();
        let mut at = 0.0;
        for (n, (column, w)) in columns.iter().zip(&widths).enumerate() {
            let right = if n == 0 {
                pinned
            } else {
                at += w;
                pinned + (at - across).clamp(0.0, room_beside)
            };
            if !native_tables_columns::is_stub(column) {
                marks.insert(column.name.clone(), 1.0 + gutter + right - inset);
            }
        }
        view.filters.laid_marks(marks);
    }
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
        if native_tables_columns::is_stub(column) {
            headers.push(native_tables_columns::stub_head(column, *w));
            continue;
        }
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
        // The name and the sort's mark keep clear of the filter's mark at the header's end.
        let filter_w = native_tables_filters::HEAD_MARK;
        let mut face = vec![fitted(
            format!("{key}/word"),
            &column.label,
            StyleRef::Note,
            (w - 20.0 - mark_w - filter_w).max(1.0),
        )];
        if let Some(mark) = mark {
            face.push(group(format!("{key}/lead")).layout(Layout::default().width(Length::Fill)));
            face.push(
                styled(format!("{key}/mark"), mark)
                    .layout(Layout::default().width(Length::Fixed(mark_w))),
            );
        }
        // How many rows fill the column: a bar along the header's foot, said in its name.
        let fill = view.fill(app, sheet, &column.name);
        let filled = view.fill_rows(app, sheet);
        headers.push(head_cell(
            format!("{key}/seat"),
            *w,
            vec![
                press(
                    &key,
                    format!(
                        "Sort by {}{said}, {}",
                        field_words(column),
                        native_tables_columns::fill_words(fill, filled)
                    ),
                ),
                group(format!("{key}/face"))
                    .layout(Layout::row().padding(Insets {
                        left: 10.0,
                        right: 10.0 + filter_w,
                        ..Insets::default()
                    }))
                    .children(face)
                    .paint_only_tree(),
                native_tables_columns::fill_bar(&key, fill, filled, *w),
                // The filter's mark, a press of its own over the header's: the header's press
                // still sorts.
                native_tables_filters::mark(view, sheet, column),
                edge(column),
            ],
        ));
    }
    // The type line: under each column's name, what its cells hold, whether it is the row's
    // key, and the table a reference names.
    let mut types = vec![head_cell("tables/type/#/seat".into(), gutter, Vec::new())];
    for (column, w) in columns.iter().zip(&widths) {
        let key = format!("tables/type/{}", column.name);
        if native_tables_columns::is_stub(column) {
            types.push(head_cell(format!("{key}/seat"), *w, Vec::new()));
            continue;
        }
        let words = crate::native_tables_choices::type_words(column, |name| app.title_of(name));
        types.push(head_cell(
            format!("{key}/seat"),
            *w,
            vec![padded(
                format!("{key}/pad"),
                fitted(key, &words, StyleRef::Note, (w - 20.0).max(1.0)),
                *w,
            )],
        ));
    }
    let room = (height - grid_chrome()).max(0.0);
    let limit = (rows.len() as f32 * line - room).max(0.0);
    let was = view.room.replace(room);
    view.limit.set(limit);
    // A grid that grows shorter (a row selected under 900 px puts its pane below; the bar of
    // changes not written comes) keeps the selected row it showed whole in view.
    if room < was
        && let Some(pk) = &view.row
        && let Some(at) = rows.iter().position(|row| row.pk == *pk)
    {
        let (top, scroll) = (at as f32 * line, view.scroll.get());
        if top >= scroll && top + line <= scroll + was {
            view.keep_in_view(at);
        }
    }
    let scroll = view.scroll.get().min(limit);
    // A cell far from what the grid shows keeps its seat and its press, which name it to the
    // keyboard and to assistive technology, and draws nothing.
    let (over_rows, over_across) = (line * 6.0, 240.0);
    let range = view.corner().and_then(|_| view.range(app));
    // Where the open cell stands on the sheet, for its list of words: its left, its top under
    // the header lines, and its width.
    let mut open_at: Option<(f32, f32, f32)> = None;
    let mut lines = Vec::with_capacity(rows.len());
    for (n, row) in rows.iter().enumerate() {
        let top = n as f32 * line;
        let near_row = top + line >= scroll - over_rows && top <= scroll + room + over_rows;
        let selected = view.row.as_deref() == Some(row.pk.as_str());
        let mut cells = Vec::with_capacity(columns.len() + 1);
        let number_key = format!("tables/num/{}", row.pk);
        let row_word = row_words[n];
        let number = if near_row {
            // On the header ground, where the note's ink reads and the tertiary one does not; a
            // row's word for a change not written in the not-written ink.
            let style = if row_word.is_some() {
                StyleRef::Unwritten
            } else if selected {
                StyleRef::Primary
            } else {
                StyleRef::Note
            };
            let words = row_word.map_or_else(|| (n + 1).to_string(), str::to_owned);
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
            // A folded group's stub is blank down its rows.
            if native_tables_columns::is_stub(column) {
                cells.push(native_tables_columns::stub_cell(column, &row.pk, *w, line));
                continue;
            }
            let at_n = at;
            // A cell a range holds beside the selected one.
            let ranged = range.as_ref().is_some_and(|(rows, columns)| {
                rows.contains(&n) && drawn_at[at].is_some_and(|at| columns.contains(&at))
            });
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
                let x = if at_n == 0 {
                    gutter
                } else {
                    gutter + pinned + left - w - across
                };
                open_at = Some((x, top - scroll, *w));
                seat.push(
                    field(EDIT, &column.label, &editing.input, view).layout(
                        Layout::default()
                            .width(Length::Fixed(*w))
                            .height(Length::Fixed(line)),
                    ),
                );
            } else {
                // The lifecycle cell carries the row's word for a change not written: a new row
                // is born live, though no cell of it says so yet.
                let life_word = row_word.filter(|_| column.name == aterm_db::typed::LIFECYCLE);
                if (staged.is_some() || life_word.is_some()) && near {
                    seat.push(styled(format!("{key}/staged"), StyleRef::Unwritten));
                }
                // A reference staged reads as the row it names, as a written one does.
                let text = staged
                    .map(|key| rows::reads(app, column, key))
                    .unwrap_or_else(|| match life_word {
                        Some(_) if rows::is_new(&row.pk) => aterm_db::typed::grid::LIVE.to_owned(),
                        _ => shown.map(|c| c.text.clone()).unwrap_or_default(),
                    });
                let conflict = staged.is_none() && shown.is_some_and(|c| !c.values.is_empty());
                // A value the save cannot take: the rule it breaks, said at the cell; else a label
                // another row claims, said in the note's ink ([`claims`]).
                let refusal = app.refusal(&at);
                let claimed = app.claimed.get(&at).filter(|_| refusal.is_none());
                let changed = app.as_of().is_some()
                    && sheet
                        .since
                        .contains_key(&(row.pk.clone(), column.name.clone()));
                // A Mac reports another value than the cell holds: marked where it stands, so a
                // mismatch shows without a press on its cell.
                let reported = sheet
                    .observed
                    .get(&(row.pk.clone(), column.name.clone()))
                    .filter(|seen| staged.is_none() && disagrees(shown, seen));
                // Who set a cell of a table written for the owner: a person's (or a program's)
                // explicit value in the primary ink, what aterm or an import set in the plain one.
                let by = shown
                    .and_then(|c| c.by.as_ref())
                    .filter(|_| !sheet.owned && staged.is_none());
                let mut spoken = format!(
                    "{} of {}: {}",
                    column.label,
                    row.pk,
                    if text.is_empty() { "empty" } else { &text }
                );
                if conflict {
                    spoken.push_str(&format!(", {} values", shown.map_or(0, |c| c.values.len())));
                }
                // A new row's word says what its lifecycle's value does not; a retire's or a
                // restore's would only say its value again.
                if let Some(word) = life_word.filter(|_| rows::is_new(&row.pk)) {
                    spoken.push_str(&format!(", {word}"));
                }
                if staged.is_some() || life_word.is_some() {
                    spoken.push_str(", not written");
                }
                if let Some(rule) = refusal {
                    spoken.push_str(&format!(", refused: {rule}"));
                }
                if let Some(claimed) = claimed {
                    spoken.push_str(&format!(", {}: {}", claims::IN_USE, claimed.words));
                }
                if changed {
                    spoken.push_str(", changed since");
                }
                if let Some(seen) = reported {
                    spoken.push_str(&format!(", {}", reports_words(seen)));
                }
                match by.map(|by| by.kind) {
                    Some(AuthorKind::Aterm) => spoken.push_str(", set by aterm"),
                    Some(AuthorKind::Import) => spoken.push_str(", imported"),
                    Some(AuthorKind::Explicit) | None => {}
                }
                if ranged {
                    spoken.push_str(", selected");
                    // Under the ring on the selected cell and the blue of a staged one.
                    if near && !focused && staged.is_none() {
                        seat.push(styled(format!("{key}/range"), StyleRef::CellRange).paint_only());
                    }
                }
                seat.push(press(&key, spoken));
                let style = match by.map(|by| by.kind) {
                    Some(AuthorKind::Explicit) => StyleRef::Primary,
                    _ => StyleRef::Plain,
                };
                if near {
                    let mut face = Vec::new();
                    if refusal.is_some()
                        || claimed.is_some()
                        || life_word.is_some()
                        || conflict
                        || changed
                        || reported.is_some()
                    {
                        let n = shown.map_or(0, |c| c.values.len());
                        // A refusal is said in the danger ink, over any other tag the cell has;
                        // then a label another row claims, in the note's ink, since the Write
                        // still takes it; then the row's word for a change not written.
                        let (tag, tag_style) = if refusal.is_some() {
                            ("refused".to_owned(), StyleRef::Danger)
                        } else if claimed.is_some() {
                            (claims::IN_USE.to_owned(), StyleRef::Note)
                        } else if let Some(word) = life_word {
                            (word.to_owned(), StyleRef::Unwritten)
                        } else if conflict {
                            (format!("{n} values"), StyleRef::Chip)
                        } else if changed {
                            ("changed".to_owned(), StyleRef::Chip)
                        } else {
                            ("differs".to_owned(), StyleRef::Chip)
                        };
                        let tag_w = text_width(&tag, SemanticRole::Text, tag_style) + 6.0;
                        // The face's padding and the gap between the words and the tag.
                        face.push(fitted(
                            format!("{key}/text"),
                            &text,
                            style,
                            (w - 24.0 - tag_w).max(1.0),
                        ));
                        face.push(words(
                            format!("{key}/tag"),
                            tag,
                            tag_style,
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
        // What left every row out: the lifecycle filter, the find, or the filters on the
        // columns' values, which the grid offers to clear.
        let find = view.find.value().trim().to_lowercase();
        let passed = sheet
            .rows
            .iter()
            .any(|row| view.life_shows(app, sheet, row) && found(app, sheet, row, &find));
        let words_now = if sheet.rows.is_empty() {
            "No rows".to_owned()
        } else if passed && !view.filters_of(sheet).is_empty() {
            String::new()
        } else if find.is_empty() {
            view.filter(sheet).none_words()
        } else {
            "No row has these words".to_owned()
        };
        if words_now.is_empty() {
            lines.push(native_tables_filters::none_pass(room_across, line));
        } else {
            lines.push(
                padded(
                    "tables/rows/none/seat".into(),
                    fitted(
                        "tables/rows/none",
                        &words_now,
                        StyleRef::Note,
                        room_across - 20.0,
                    ),
                    room_across,
                )
                .layout_height(line),
            );
        }
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
            lane("tables/types".into(), types, head_line, true),
            body,
        ]);
    // Over the sheet, where columns lie off its side: a thumb along its foot that says how
    // many and where they stand, and a bar down the key column's edge while some lie under it.
    let mut over = vec![sheet_lines];
    // The open cell's list of words, under it (over it where the sheet's foot is nearer).
    if let Some((x, top, w)) = open_at
        && let Some(editing) = view.editing.as_ref()
        && let Some(choice) = editing.choice.as_ref()
    {
        let under = 1.0 + 2.0 * head_line + line;
        let label = sheet
            .columns
            .iter()
            .find(|c| c.name == editing.column)
            .map_or_else(|| editing.column.clone(), field_words);
        over.push(crate::native_tables_choices::list(
            choice,
            &format!("Choices for {label}"),
            editing.input.value(),
            (x, under + top, w),
            (room + under, line),
        ));
    }
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

/// The cell bar: a group named for what it shows.
const CELL_BAR: &str = "tables/cellbar";
/// The room between the cell bar's seats, and between the runs of one seat.
const BAR_GAP: f32 = 12.0;
const RUN_GAP: f32 = 4.0;

/// Words of the cell bar in one ink.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Run {
    text: String,
    style: StyleRef,
}

fn run(text: impl Into<String>, style: StyleRef) -> Run {
    Run {
        text: text.into(),
        style,
    }
}

/// What the cell bar says of the selection, seat by seat.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct BarWords {
    /// Which cell, or how many: the first seat.
    address: Vec<Run>,
    /// The cell's value as the grid shows it; none for a range, for no selection and for a
    /// cell the grid does not show.
    value: Option<Run>,
    /// The cell's state, where the grid marks one: not written, with what the registry holds
    /// (and `refused` first where the check or a Write refuses it); conflicted, or written by
    /// a newer aterm; a Mac reports another value; changed since (with who wrote what it
    /// holds now); a range's count of cells and of filled ones and, over whole numbers, their
    /// sum, least and most; that the find leaves the cell's row out. None for a linked count,
    /// whose count is its value. Never who wrote the value the cell holds or when: the line
    /// under the grid says where that value came from ([`provenance`]).
    state: Vec<Run>,
}

/// The last `chars` characters of `key`: all of it where it has no more.
fn ending(key: &str, chars: usize) -> &str {
    let from = match chars {
        0 => key.len(),
        n => key.char_indices().rev().nth(n - 1).map_or(0, |(at, _)| at),
    };
    &key[from..]
}

/// How many characters `a` and `b` end in alike.
fn ends_alike(a: &str, b: &str) -> usize {
    a.chars()
        .rev()
        .zip(b.chars().rev())
        .take_while(|(a, b)| a == b)
        .count()
}

/// What of `key` tells its row apart from `twins`, the keys of the other rows that show the
/// same words: `…0002`, the key's last four characters, where no twin's key ends so. Where one
/// does (the rows a Mac states of a collection keyed `<row>.<the Mac's id>` all end in that
/// id), the ending this key and those twins all share says nothing, and the mark is the last
/// of what stands before it, `…aaaa…`: four characters, or as many more as no such twin has
/// there. That is followed by the key's last four (`…aaaa…505b`) only where some twin ends
/// otherwise, so that no two rows of the same words ever read the same mark.
fn told_apart(key: &str, twins: &[&str]) -> String {
    let last = ending(key, 4);
    let alike: Vec<&str> = twins
        .iter()
        .copied()
        .filter(|twin| twin.ends_with(last))
        .collect();
    if alike.is_empty() {
        return format!("\u{2026}{last}");
    }
    let shared = alike
        .iter()
        .map(|twin| ends_alike(key, twin))
        .min()
        .unwrap_or(0);
    // What stands in a key before the ending they share.
    let before = |of: &str| of.len() - ending(of, shared).len();
    let own = &key[..before(key)];
    // One character more than the longest ending a twin has there in common with this key.
    let needs = alike
        .iter()
        .map(|twin| ends_alike(own, &twin[..before(twin)]) + 1)
        .max()
        .unwrap_or(0)
        .max(4);
    let part = ending(own, needs);
    if part.is_empty() {
        // The whole key is the ending of each such twin's: nothing shorter is its own.
        return key.to_owned();
    }
    let lead = if part.len() < own.len() {
        "\u{2026}"
    } else {
        ""
    };
    if alike.len() == twins.len() {
        format!("{lead}{part}\u{2026}")
    } else {
        format!("{lead}{part}\u{2026}{last}")
    }
}

/// A selected cell's address: its row's words in the sheet's key column, then its column's.
/// A row whose key cell is empty is named by its own key. Where another row of the sheet shows
/// the same words, what of this row's key tells it apart from them follows, dimmed
/// ([`told_apart`]).
fn address_runs(sheet: &Sheet, row: &SheetRow, column: &SheetColumn) -> Vec<Run> {
    fn named<'a>(sheet: &Sheet, row: &'a SheetRow) -> &'a str {
        row.cells
            .get(&sheet.key)
            .map_or("", |cell| cell.text.as_str())
    }
    let name = named(sheet, row);
    // The column's words are a run of their own, the last: a line too narrow for the address
    // cuts the row's words before them ([`run_rooms`]).
    let column = run(format!("\u{b7} {}", field_words(column)), StyleRef::Primary);
    // A row added here and not yet written has no key a person knows: it is the new row, as
    // its pane's title says ([`rows::is_new`]).
    if rows::is_new(&row.pk) {
        return vec![
            run(
                format!("New {}", sheet.singular.to_lowercase()),
                StyleRef::Primary,
            ),
            column,
        ];
    }
    if name.is_empty() {
        return vec![run(row.pk.as_str(), StyleRef::Primary), column];
    }
    let twins: Vec<&str> = sheet
        .rows
        .iter()
        .filter(|other| other.pk != row.pk && named(sheet, other) == name)
        .map(|other| other.pk.as_str())
        .collect();
    if twins.is_empty() {
        return vec![run(name, StyleRef::Primary), column];
    }
    vec![
        run(name, StyleRef::Primary),
        run(told_apart(&row.pk, &twins), StyleRef::Faint),
        column,
    ]
}

/// What a seat's runs say as one line: the seat's spoken words.
fn spoken(runs: &[Run]) -> String {
    runs.iter()
        .map(|run| run.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

/// A range's words: how many cells it holds and how many show something; where `numbers` (each
/// of its columns holds whole numbers) and at least one cell reads as one, their sum, the least
/// and the most. A sum past what the page counts in is not said.
fn range_words(texts: &[&str], numbers: bool) -> String {
    let filled = texts.iter().filter(|text| !text.is_empty()).count();
    let mut words = format!("{} cells \u{b7} {filled} filled", texts.len());
    if numbers {
        let read: Vec<i128> = texts
            .iter()
            .filter_map(|text| text.trim().parse().ok())
            .collect();
        let sum = read.iter().try_fold(0_i128, |sum, n| sum.checked_add(*n));
        if let (Some(sum), Some(least), Some(most)) = (sum, read.iter().min(), read.iter().max()) {
            words.push_str(&format!(
                " \u{b7} sum {sum} \u{b7} least {least} \u{b7} most {most}"
            ));
        }
    }
    words
}

/// What a Mac reports of a cell, as the grid's cell speaks it and the cell bar says it:
/// `m55 reports 26.1`.
fn reports_words(seen: &Observed) -> String {
    format!("{} reports {}", mac_of(&seen.by), value_words(&seen.text))
}

/// What a cell that changed after a past point holds now, and who wrote that.
fn since_runs(since: &Since) -> Vec<Run> {
    let mut runs = vec![run(
        if since.members {
            format!("since {}", since.now)
        } else {
            format!("now {}", value_words(&since.now))
        },
        StyleRef::Note,
    )];
    if !since.by.is_empty() {
        let who = since
            .by
            .iter()
            .map(Author::who)
            .collect::<Vec<_>>()
            .join(" / ");
        // One writer is set as its label is known; several in the note's own style.
        let style = match since.by.as_slice() {
            [one] => one.style(StyleRef::Note),
            _ => StyleRef::Note,
        };
        runs.push(run(format!("\u{b7} {who}"), style));
    }
    runs
}

/// What the cell bar says of a selected cell whose row the find field leaves out of the grid.
const NOT_FOUND: &str = "not among the rows found";
/// What it says where the find passes the row and the filters on the columns' values leave it
/// out.
const FILTERED: &str = "left out by the filters";

/// What the cell bar says of selected `row`, which the grid does not show: why, in the words of
/// what leaves it out, the find first ([`NOT_FOUND`]), then the filters on the columns' values
/// ([`FILTERED`]), then the lifecycle filter (`not among the live rows`).
fn left_out_words(
    app: &TablesApp,
    view: &TablesViewState,
    sheet: &Sheet,
    row: &SheetRow,
) -> String {
    if !found(app, sheet, row, &view.find.value().trim().to_lowercase()) {
        NOT_FOUND.to_owned()
    } else if native_tables_filters::leave_out(app, sheet, &view.filters_of(sheet), row) {
        FILTERED.to_owned()
    } else if !view.life_shows(app, sheet, row) {
        view.filter(sheet).left_out_words()
    } else {
        // Nothing leaves the row out, so only its cell's column is not drawn, a selection the
        // columns' settling moves to a column drawn ([`TablesViewState::settle_columns`]).
        NOT_FOUND.to_owned()
    }
}

/// The cell bar's words for the selection: a range's size and what it holds, the selected
/// cell's address, value and state, or that no cell is selected. Every
/// value is the grid's own text for its cell (a staged cell's staged words), so a secret's
/// cell reads its entry name or its mask here as it does there, and a cell a Mac reports
/// another value of reads the value it holds, as the grid's cell does. The cell whose value
/// and state are said is one the grid shows ([`TablesViewState::stands`]), the one a copy or a
/// Delete acts on. A selection whose row the grid leaves out (the find field, the filters on the
/// columns' values or the lifecycle filter) is still the selection the line under the grid and
/// the pane tell of, so the bar names it by its address and says only what leaves it out
/// ([`left_out_words`]); a selection that names no cell of the sheet (none, or a row a new read
/// no longer holds) is no cell here, as it is none to that line.
///
/// The state is what the grid marks on the cell, in the grid's order: a staged cell is not
/// written, and `refused` first where the check or a Write refused it ([`TablesApp::refusal`]),
/// as the grid's tag says it over every other (nothing else is said of it, as its grid cell
/// wears no other mark); else, where another row claims a staged label, `in use` first and
/// what the label already names and what this row reads as once written ([`claims`]); a conflict says how many values; a linked count has none, its
/// sentence being the line under the grid's; a Mac that reports another value is named with
/// what it reports ([`reports_words`], the words the grid's cell speaks, under [`disagrees`],
/// the grid's own rule); in a past table a cell changed since says what it holds now and who
/// wrote that ([`since_runs`]). Who wrote the value the cell holds, and when, is not said
/// here: the line under the grid says it ([`provenance`]).
fn bar_words(app: &TablesApp, view: &TablesViewState, sheet: &Sheet) -> BarWords {
    // The staged words of this sheet, by row and column.
    let staged: BTreeMap<(&str, &str), &str> = app
        .staged
        .iter()
        .filter(|((of, ..), _)| of == sheet.name)
        .map(|((_, pk, column), text)| ((pk.as_str(), column.as_str()), text.as_str()))
        .collect();
    // A range, while its rows stand as they did when it was grown.
    if let Some((rows, across)) = view.range(app).filter(|_| view.corner().is_some()) {
        let shown = view.shown_rows(app, sheet);
        let columns = view.shown_columns(app, sheet);
        let (tall, wide) = (rows.clone().count(), across.clone().count());
        if tall * wide > 1 {
            let texts: Vec<&str> = rows
                .flat_map(|row| across.clone().map(move |column| (row, column)))
                .filter_map(|(row, column)| Some((*shown.get(row)?, *columns.get(column)?)))
                .map(|(row, column)| {
                    staged
                        .get(&(row.pk.as_str(), column.name.as_str()))
                        .copied()
                        .unwrap_or_else(|| {
                            row.cells
                                .get(&column.name)
                                .map_or("", |cell| cell.text.as_str())
                        })
                })
                .collect();
            let numbers = across.clone().all(|column| {
                columns
                    .get(column)
                    .is_some_and(|c| c.kind == ColumnKind::Number)
            });
            return BarWords {
                address: vec![run(
                    format!("{tall} \u{d7} {wide} cells"),
                    StyleRef::Primary,
                )],
                value: None,
                state: vec![run(range_words(&texts, numbers), StyleRef::Note)],
            };
        }
    }
    let selected = view.stands(app).and_then(|(row, column, ..)| {
        Some((
            *view.shown_rows(app, sheet).get(row)?,
            *view.shown_columns(app, sheet).get(column)?,
        ))
    });
    let Some((row, column)) = selected else {
        // A selected cell whose row the grid leaves out: the line under the grid and the pane
        // still tell of it ([`provenance`] finds it as this does), so the bar names it too,
        // and says what leaves it out. Its value is not said: the grid shows none.
        let hidden = view.row.as_ref().zip(view.column.as_ref()).and_then(|at| {
            Some((
                sheet.rows.iter().find(|row| row.pk == *at.0)?,
                sheet.columns.iter().find(|column| column.name == *at.1)?,
            ))
        });
        return match hidden {
            Some((row, column)) => BarWords {
                address: address_runs(sheet, row, column),
                value: None,
                state: vec![run(left_out_words(app, view, sheet, row), StyleRef::Note)],
            },
            None => BarWords {
                address: vec![run("No cell selected", StyleRef::Note)],
                ..BarWords::default()
            },
        };
    };
    let shown = row.cells.get(&column.name);
    let stored = shown.map_or("", |cell| cell.text.as_str());
    let mut state = Vec::new();
    let value = if let Some(text) = staged.get(&(row.pk.as_str(), column.name.as_str())) {
        // A value the check or a Write refused reads so first, in the grid's own word and ink;
        // the rule it breaks is the pane's to say.
        let unwritten = format!("not written \u{b7} holds {}", value_words(stored));
        let at = (sheet.name.to_owned(), row.pk.clone(), column.name.clone());
        if app.refusal(&at).is_some() {
            state.push(run("refused", StyleRef::Danger));
            state.push(run(format!("\u{b7} {unwritten}"), StyleRef::Note));
        } else if let Some(claimed) = app.claimed.get(&at) {
            // A label another row claims: the grid's word, then what it already names and what
            // this row reads as once written, all in the note's ink.
            state.push(run(claims::IN_USE, StyleRef::Note));
            state.push(run(format!("\u{b7} {}", claimed.words), StyleRef::Note));
            state.push(run(format!("\u{b7} {unwritten}"), StyleRef::Note));
        } else {
            state.push(run(unwritten, StyleRef::Note));
        }
        run(typed_words(column, text), StyleRef::Unwritten)
    } else {
        // A linked count has no state here: the count is its value, and how many rows of
        // which table it counts is the line under the grid's to say, as the type line names
        // the table.
        if let Some(shown) = shown.filter(|cell| !cell.values.is_empty()) {
            state.push(run(
                format!("{} values \u{b7} no clock picks one", shown.values.len()),
                StyleRef::Note,
            ));
        } else if shown.is_some_and(|cell| cell.raw) {
            state.push(run(
                "written by a newer aterm \u{b7} read only here",
                StyleRef::Note,
            ));
        }
        // Each later state follows the one before it after a middle dot.
        let after = |state: &[Run], words: String| {
            if state.is_empty() {
                words
            } else {
                format!("\u{b7} {words}")
            }
        };
        // A Mac reports another value than the cell holds: the grid's mark and its words.
        let at = (row.pk.clone(), column.name.clone());
        if let Some(seen) = sheet
            .observed
            .get(&at)
            .filter(|seen| disagrees(shown, seen))
        {
            let words = after(&state, format!("differs \u{b7} {}", reports_words(seen)));
            state.push(run(words, StyleRef::Note));
        }
        // In a past table a cell changed since says what it holds now and who wrote that.
        if let Some(since) = app.as_of().and_then(|_| sheet.since.get(&at)) {
            let mut runs = since_runs(since);
            if let Some(first) = runs.first_mut() {
                first.text = after(&state, std::mem::take(&mut first.text));
            }
            state.extend(runs);
        }
        if stored.is_empty() {
            run("empty", StyleRef::Faint)
        } else {
            run(stored, StyleRef::Plain)
        }
    };
    BarWords {
        address: address_runs(sheet, row, column),
        value: Some(value),
        state,
    }
}

/// How wide `runs` set on one line are, the room between them counted.
fn runs_width(runs: &[Run]) -> f32 {
    let words: f32 = runs
        .iter()
        .map(|run| text_width(&run.text, SemanticRole::Text, run.style))
        .sum();
    words + RUN_GAP * runs.len().saturating_sub(1) as f32
}

/// How wide the cell bar's address seat is for `sheet` whichever cell is selected: room for
/// the widest of its first rows' keys beside the widest of its columns' words.
fn address_room(sheet: &Sheet) -> f32 {
    let key = sheet
        .rows
        .iter()
        .take(200)
        .filter_map(|row| row.cells.get(&sheet.key))
        .map(|cell| text_width(&cell.text, SemanticRole::Text, StyleRef::Primary))
        .fold(0.0_f32, f32::max);
    let column = sheet
        .columns
        .iter()
        .map(|column| {
            text_width(
                &format!("\u{b7} {}", field_words(column)),
                SemanticRole::Text,
                StyleRef::Primary,
            )
        })
        .fold(0.0_f32, f32::max);
    key + RUN_GAP + column
}

/// The characters a cut first run keeps before its ellipsis, where it has more than these.
const RUN_KEEPS: usize = 4;

/// The room each of `runs` takes on one line of `width`. Where the line is too narrow for all
/// of their words the first run is cut first, down to its first characters and an ellipsis;
/// then the last run, then each before it; and the first again only once the others are gone.
/// The rooms together are never wider than `width` less the gaps between the runs, down to
/// nothing.
fn run_rooms(runs: &[Run], width: f32) -> Vec<f32> {
    let mut rooms: Vec<f32> = runs
        .iter()
        .map(|run| text_width(&run.text, SemanticRole::Text, run.style))
        .collect();
    let gaps = RUN_GAP * runs.len().saturating_sub(1) as f32;
    let mut over = (rooms.iter().sum::<f32>() + gaps - width).max(0.0);
    if let (Some(first), Some(room)) = (runs.first(), rooms.first_mut()) {
        let least = if first.text.chars().count() > RUN_KEEPS + 1 {
            let kept: String = first.text.chars().take(RUN_KEEPS).collect();
            text_width(
                &format!("{}\u{2026}", kept.trim_end()),
                SemanticRole::Text,
                first.style,
            )
            .min(*room)
        } else {
            *room
        };
        let cut = over.min(*room - least);
        *room -= cut;
        over -= cut;
    }
    for room in rooms.iter_mut().skip(1).rev() {
        let cut = over.min(*room);
        *room -= cut;
        over -= cut;
    }
    if let Some(room) = rooms.first_mut() {
        *room = (*room - over).max(0.0);
    }
    rooms
}

/// The room each of `runs` takes on one line of `width` that is cut at its end: each run its
/// own width while the line has it, the first without room cut to what is left, and none
/// after it.
fn rooms_to_end(runs: &[Run], width: f32) -> Vec<f32> {
    let mut left = width.max(0.0);
    runs.iter()
        .map(|run| {
            let room = text_width(&run.text, SemanticRole::Text, run.style).min(left);
            left = (left - room - RUN_GAP).max(0.0);
            room
        })
        .collect()
}

/// `runs` on one line, each a painted text keyed `key`, then `key/1`, `key/2`, in its room of
/// `rooms` ([`run_rooms`], [`rooms_to_end`]): a run cut there ends in an ellipsis, and one
/// with no room for an ellipsis is left out with the gap before it, so no run paints past the
/// line. The seat they stand in speaks their words ([`seat`]).
fn run_nodes(key: &str, runs: &[Run], rooms: Vec<f32>) -> Vec<UiNode> {
    let mut nodes = Vec::with_capacity(runs.len() * 2);
    for (n, (run, room)) in runs.iter().zip(rooms).enumerate() {
        if room < text_width("\u{2026}", SemanticRole::Text, run.style) {
            continue;
        }
        if !nodes.is_empty() {
            nodes.push(
                group(format!("{key}/{n}/gap"))
                    .layout(Layout::default().width(Length::Fixed(RUN_GAP))),
            );
        }
        let key = if n == 0 {
            key.to_owned()
        } else {
            format!("{key}/{n}")
        };
        nodes.push(fitted(key, &run.text, run.style, room));
    }
    nodes
}

/// One seat of the cell bar, `width` wide: a text that speaks `whole`, the seat's words uncut,
/// over `painted`, the words the line has room for. What the line cuts or leaves out is still
/// spoken, as a grid cell's press speaks the words its face cuts.
fn seat(key: String, whole: String, width: f32, painted: Vec<UiNode>) -> UiNode {
    UiNode::new(
        key,
        UiContent::Group(GroupSpec {
            label: Some(whole),
            role: SemanticRole::Text,
            style: StyleRef::Plain,
        }),
    )
    .layout(Layout::row().width(Length::Fixed(width)).clipped())
    .children(painted.into_iter().map(UiNode::paint_only_tree).collect())
}

/// Of the bar's room, what the value keeps before the address is cut for it: its own words, as
/// far as this share of the bar.
const VALUE_KEEPS: f32 = 0.55;

/// The cell bar ([`CELL_BAR`]): one ruled line over the grid that says which cell is selected,
/// its whole value and its state where the grid marks one ([`bar_words`]); what its column
/// holds is the type line's to say, under the column's name
/// ([`crate::native_tables_choices::type_words`]). It is there whether or not a cell is selected or a change is staged, so
/// the grid never moves for either; only a window too short for it over the grid's own lines
/// and a whole row has none ([`page`]).
///
/// The address stands at its own width; the value takes the room beside it and the state's
/// words what the value leaves, cut at their end, and are left out where that is no room for
/// an ellipsis. Only a value with no room beside the whole address cuts it, and then no
/// further than leaves the value [`VALUE_KEEPS`] of the bar; the value itself is cut at the
/// bar's end. Each seat speaks its words whole ([`seat`]), so the group reads as one sentence:
/// which cell, its value, its state.
fn cell_bar(app: &TablesApp, view: &TablesViewState, sheet: &Sheet, width: f32) -> UiNode {
    let said = bar_words(app, view, sheet);
    // A rule at each side, and ten points inside each.
    let inner = (width - 22.0).max(1.0);
    // What stands first beside the address: the cell's value, or a range's words.
    let (lead_w, state_w) = match &said.value {
        Some(value) => (
            text_width(&value.text, SemanticRole::Text, value.style),
            runs_width(&said.state),
        ),
        None => (runs_width(&said.state), 0.0),
    };
    let after = |width: f32| if width > 0.0 { width + BAR_GAP } else { 0.0 };
    // The address's edge and the ten points after it, where anything stands beside it.
    let edge = if lead_w > 0.0 { 11.0 } else { 0.0 };
    // The address at its own width, cut only for the room the value keeps.
    let keeps = lead_w.min(inner * VALUE_KEEPS);
    let named = runs_width(&said.address).min((inner - edge - keeps).max(1.0));
    let most = (inner - named - edge).max(1.0);
    let lead_w = lead_w.min(most);
    // The state's words in what the value leaves, cut at their end; none where that is no
    // room for their first run's ellipsis.
    let state_room = state_w.min(most - lead_w - BAR_GAP);
    let state_runs = if state_w > 0.0 && state_room > 0.0 {
        run_nodes(
            &format!("{CELL_BAR}/state/words"),
            &said.state,
            rooms_to_end(&said.state, state_room),
        )
    } else {
        Vec::new()
    };
    let state_w = if state_runs.is_empty() {
        0.0
    } else {
        state_room
    };
    // The address's seat then takes what is left, up to the sheet's widest address (never past
    // 45% of the bar), so the value starts at one place whichever cell is selected while there
    // is room for that.
    let beside = lead_w + after(state_w);
    let steady = address_room(sheet).min(inner * 0.45).max(named);
    let address_w = (inner - edge - beside).clamp(named, steady);
    let mut seats = vec![seat(
        format!("{CELL_BAR}/address"),
        spoken(&said.address),
        address_w,
        run_nodes(
            &format!("{CELL_BAR}/address/words"),
            &said.address,
            run_rooms(&said.address, named),
        ),
    )];
    if lead_w > 0.0 {
        seats.push(
            styled(format!("{CELL_BAR}/edge"), StyleRef::Cell)
                .layout(Layout::default().width(Length::Fixed(1.0))),
        );
        seats.push(
            group(format!("{CELL_BAR}/lead")).layout(Layout::default().width(Length::Fixed(10.0))),
        );
    }
    match &said.value {
        Some(value) => {
            seats.push(seat(
                format!("{CELL_BAR}/value"),
                value.text.clone(),
                lead_w,
                vec![fitted(
                    format!("{CELL_BAR}/value/words"),
                    &value.text,
                    value.style,
                    lead_w,
                )],
            ));
            if !state_runs.is_empty() {
                seats.push(
                    group(format!("{CELL_BAR}/state/gap"))
                        .layout(Layout::default().width(Length::Fixed(BAR_GAP))),
                );
                seats.push(seat(
                    format!("{CELL_BAR}/state"),
                    spoken(&said.state),
                    state_w,
                    state_runs,
                ));
            }
        }
        // A range's words stand in the value's place, cut at the bar's end.
        None if lead_w > 0.0 => seats.push(seat(
            format!("{CELL_BAR}/state"),
            spoken(&said.state),
            lead_w,
            run_nodes(
                &format!("{CELL_BAR}/state/words"),
                &said.state,
                rooms_to_end(&said.state, lead_w),
            ),
        )),
        None => {}
    }
    seats.push(group(format!("{CELL_BAR}/space")).layout(Layout::default().width(Length::Fill)));
    // The frame: a rule down each side and along the top; the grid's own top rule closes it.
    UiNode::new(CELL_BAR, UiContent::Group(GroupSpec::new("Selected cell")))
        .layout(Layout::row())
        .children(vec![
            styled(format!("{CELL_BAR}/left"), StyleRef::Cell)
                .layout(Layout::default().width(Length::Fixed(1.0))),
            group(format!("{CELL_BAR}/body"))
                .layout(Layout::column())
                .children(vec![
                    styled(format!("{CELL_BAR}/top"), StyleRef::Cell)
                        .layout(Layout::default().height(Length::Fixed(1.0))),
                    group(format!("{CELL_BAR}/seats"))
                        .layout(Layout::row().padding(Insets::symmetric(10.0, 0.0)))
                        .children(seats),
                ]),
            styled(format!("{CELL_BAR}/right"), StyleRef::Cell)
                .layout(Layout::default().width(Length::Fixed(1.0))),
        ])
}

fn write_bar(app: &TablesApp, width: f32, reading_more: bool) -> Option<UiNode> {
    let h = 40.0 * scale().max(1.0);
    // Where this Mac cannot write, the bar's place says why, and nothing offers a Write.
    if let Some(why) = app.cannot_write() {
        return Some(rows::cannot_write(app, why, width, h));
    }
    let n = app.staged.len();
    if n == 0 {
        let note = if reading_more {
            "Reading remaining tables…"
        } else {
            app.note.as_deref()?
        };
        return Some(
            fitted("tables/note", note, StyleRef::Note, width - 20.0).layout(
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
    // The cells the check refuses, which a Write holds back with the rest of their table.
    let counted = match app.checked.len() {
        0 => count.clone(),
        refused => format!("{count} \u{b7} {refused} refused"),
    };
    let discard = button(
        DISCARD,
        "Discard",
        "Discard every change not written",
        StyleRef::Secondary,
        !app.writing,
    );
    // The name a reader hears starts with the words the button shows.
    let write_label = if app.writing {
        "Writing\u{2026}"
    } else {
        "Write"
    };
    let write = button(
        WRITE,
        write_label,
        &format!("{write_label} {count}"),
        StyleRef::Unwritten,
        !app.writing,
    );
    let buttons_w = crate::native_ui::native_button_label_width("Discard")
        + crate::native_ui::native_button_label_width("Writing\u{2026}")
        + 48.0
        + 8.0;
    let count_w = text_width(&counted, SemanticRole::Text, StyleRef::Unwritten);
    let note_w = (width - 28.0 - count_w - buttons_w - 24.0).max(0.0);
    let mut children = vec![fitted(
        "tables/bar/count",
        &counted,
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

/// What is typed into `column`, staged and not written, in the words its cell says a held value
/// by: a time's number as its date ([`stamp_words`]), as the registry's holds reads beside it;
/// else as typed ([`value_words`]).
fn typed_words(column: &SheetColumn, typed: &str) -> String {
    match column.compiled.map(|c| &c.ty) {
        Some(aterm_db::typed::Ty::Time) => typed
            .trim()
            .parse::<u64>()
            .map_or_else(|_| value_words(typed), stamp_words),
        _ => value_words(typed),
    }
}

/// A field's name in the pane: `Nickname`, `Ram gb`.
fn field_words(column: &SheetColumn) -> String {
    sentence(&column.label)
}

/// `words` in sentence case, words apart: `RAM_GB` and `ram_gb` read `Ram gb`.
pub(crate) fn sentence(words: &str) -> String {
    let lower = words.replace('_', " ").to_lowercase();
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
    let tall = Cell::new(0.0_f32);
    let push = |children: &mut Vec<UiNode>, node: UiNode, h: f32| {
        children.push(node.layout_height(h));
        tall.set(tall.get() + h + 4.0);
    };
    // The Keep and Undo buttons laid out, top down, which the arrows move among.
    let mut actions: Vec<(String, f32, f32)> = Vec::new();
    view.pane_keys.borrow_mut().clear();
    let Some(row) = view
        .row
        .as_ref()
        .and_then(|pk| sheet.rows.iter().find(|r| r.pk == *pk))
    else {
        push(
            &mut children,
            fitted(
                "tables/pane/none",
                "Select a row to see its fields",
                StyleRef::Note,
                inner,
            ),
            line,
        );
        return pane_frame(children, width, height, view, tall.get());
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
    let new = rows::is_new(&row.pk);
    // A new row is named as one until it is written, so a link's is not read as its machine.
    let name = if new {
        format!("New {}", sheet.singular.to_lowercase())
    } else {
        rows::row_name(app, sheet, row, None).unwrap_or_else(|| row.pk.clone())
    };
    // A new row has no key until the save derives it.
    let id = if new {
        "Its key is made when it is written".to_owned()
    } else {
        row.pk.clone()
    };
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
        fitted("tables/pane/id", &id, StyleRef::Note, inner),
        line,
    );
    // The row's lifecycle: what retiring, ending or restoring it does, what holds a retire back,
    // and what a new row cannot be given here.
    // Then each row its references name, a press away.
    for (node, h, action) in rows::pane_lines(app, sheet, row, inner, line)
        .into_iter()
        .chain(rows::reference_lines(app, sheet, row, inner, line))
        .chain(claims::pane_lines(app, sheet, row, inner, line))
    {
        if let Some(action) = action {
            actions.push((action, tall.get(), tall.get() + h));
        }
        push(&mut children, node, h);
    }
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
        for (shown_at, (n, (value, by))) in ordered_values.into_iter().enumerate() {
            // Keep stages a value, so it is offered only where the page writes the cell. Its line
            // stands under the box's padding and head, one line and gap for each value above.
            let keep = app.keepable(sheet.name, &row.pk, &column.name).then(|| {
                let top = tall.get() + 10.0 + line + 4.0 + shown_at as f32 * (line + 8.0);
                actions.push((format!("{KEEP}{}/{n}", column.name), top, top + line + 4.0));
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
    for group_of in Group::ALL {
        let members: Vec<&&SheetColumn> = columns.iter().filter(|c| c.group == group_of).collect();
        if members.is_empty() {
            continue;
        }
        let head = format!(
            "tables/pane/group/{}",
            crate::native_ui::key_word(group_of.word())
        );
        let mut head_line = vec![fitted(
            format!("{head}/word"),
            group_of.word(),
            StyleRef::Note,
            inner - meta_w,
        )];
        if group_of != Group::Linked && !sheet.owned {
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
            // A new row is born live, as its lifecycle cell in the grid says, though nothing is
            // staged there.
            let born_live = new && column.name == aterm_db::typed::LIFECYCLE;
            if staged.is_none()
                && !born_live
                && shown.is_none_or(|c| c.text.is_empty())
                && app.refusal(&at).is_none()
            {
                empty.push(field_words(column));
                continue;
            }
            let text = if born_live {
                aterm_db::typed::grid::LIVE.to_owned()
            } else {
                staged.map_or_else(
                    || shown.map(|c| c.text.clone()).unwrap_or_default(),
                    |key| rows::reads(app, column, key),
                )
            };
            let value_style = if staged.is_some() || born_live {
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
                actions.push((
                    format!("{UNDO}{}", column.name),
                    tall.get(),
                    tall.get() + line + 2.0,
                ));
                parts.push(button(
                    &format!("{UNDO}{}", column.name),
                    "Undo",
                    &format!("Undo {}", field_words(column)),
                    StyleRef::Secondary,
                    true,
                ));
            } else if group_of != Group::Linked
                && !sheet.owned
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
            if let Some(refused) = app.refusal(&at) {
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
                &format!(
                    "tables/pane/empty/{}",
                    crate::native_ui::key_word(group_of.word())
                ),
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
    // The row's history is the drawer's: the pane opens it on this row, wherever the drawer
    // stands (in every window but one too short for its line and three lines of grid).
    if view.history.drawn.get() {
        push(
            &mut children,
            group("tables/pane/history/seat")
                .layout(Layout::row())
                .children(vec![button(
                    history::OF_ROW,
                    "History of this row",
                    "History of this row",
                    StyleRef::Secondary,
                    true,
                )]),
            line + 6.0,
        );
    }
    *view.pane_keys.borrow_mut() = actions;
    pane_frame(children, width, height, view, tall.get())
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
    view.pane_room.set(room);
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

/// What the line under the grid says of the selected cell's value: where it came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Said {
    /// A Mac's statement of what it saw: when it saw it, and which Mac.
    Seen,
    /// What was written at a Mac for the owner, explicitly (a person at the page or a terminal,
    /// or a program), and when.
    Set,
    /// What aterm wrote by itself for the owner's table at a Mac (a row's birth, a compiled
    /// seed), and when: neither a person's setting nor an observation.
    Automatic,
    /// What an import wrote, at which Mac and when.
    Imported,
    /// A held value the newest observation of it disagrees with: each is said, and neither is
    /// written over the other.
    Differs,
    /// Several values written without seeing each other: each with its writer.
    Values,
    /// How many rows of another table name this row.
    Linked,
    /// The row's own key, fixed when the row was born.
    Key,
    /// Typed here and not yet written.
    Unwritten,
    /// Nothing selected, or nothing recorded of the cell.
    Nothing,
}

impl Said {
    /// The word set before the line, so no meaning rests on its ink alone.
    const fn tag(self) -> &'static str {
        match self {
            Self::Seen => "SEEN",
            Self::Set => "SET",
            Self::Automatic => "AUTOMATIC",
            Self::Imported => "IMPORTED",
            Self::Differs => "DIFFERS",
            Self::Values => "VALUES",
            Self::Linked => "LINKED",
            Self::Key => "KEY",
            Self::Unwritten => "NOT WRITTEN",
            Self::Nothing => "",
        }
    }

    /// How the line's words are set: what a person, an import or aterm set, and what is typed
    /// and not written, read in the plain ink; an observation in the note's; a disagreement in
    /// the primary ink, the strongest the page has that is no colour (every mark but the blue
    /// of not written is neutral). The staged cell itself wears the blue, so the line does not.
    const fn style(self) -> StyleRef {
        match self {
            Self::Differs => StyleRef::Primary,
            Self::Set | Self::Automatic | Self::Imported | Self::Values | Self::Unwritten => {
                StyleRef::Plain
            }
            Self::Seen | Self::Linked | Self::Key | Self::Nothing => StyleRef::Note,
        }
    }

    /// How the word before the line is set: a disagreement in the primary ink, so it stands
    /// apart from every other word at a glance; the rest as a chip.
    const fn tag_style(self) -> StyleRef {
        match self {
            Self::Differs => StyleRef::Primary,
            _ => StyleRef::Chip,
        }
    }
}

/// The Mac a writer is: its label, or the start of its machine id where none is known.
fn mac_of(by: &Author) -> &str {
    by.label.as_deref().unwrap_or(&by.id)
}

/// How long ago `at_ms` was: `just now`, `4 min ago`, `3 h ago`, or `on Sat 7 Nov 19:12` for a
/// day or more, or a claim later than this Mac's clock.
pub(crate) fn ago_words(at_ms: u64, now_ms: u64) -> String {
    if at_ms > now_ms.saturating_add(60_000) {
        return format!("on {}", date_words(at_ms));
    }
    let gone = now_ms.saturating_sub(at_ms) / 1000;
    match gone {
        0..60 => "just now".to_owned(),
        60..3_600 => format!("{} min ago", gone / 60),
        3_600..86_400 => format!("{} h ago", gone / 3_600),
        _ => format!("on {}", date_words(at_ms)),
    }
}

/// `just now`, or `on Sat 7 Nov 19:12`, a claim later than this Mac's clock by its date.
pub(crate) fn on_words(at_ms: u64, now_ms: u64) -> String {
    if at_ms > now_ms.saturating_add(60_000) {
        return format!("on {}", date_words(at_ms));
    }
    match when_words(at_ms, now_ms) {
        now if now == "just now" => now,
        day => format!("on {day}"),
    }
}

/// How a writer set a cell, in the line's words: `set at m55 on …`, `cleared at m55 …`, `set by
/// aterm at m22 …`, `imported at m55 …`, `set by claude at m55 …`. A person's or a
/// program's explicit write is named by the Mac it was made at, and by who made it where its
/// record says (a version 2 record, design §6); what aterm or an import wrote says so.
fn set_words(by: &Author, cleared: bool, now_ms: u64) -> (Said, String) {
    let verb = if cleared { "cleared" } else { "set" };
    let (said, verb) = match by.kind {
        AuthorKind::Import => (Said::Imported, "imported".to_owned()),
        AuthorKind::Aterm => (Said::Automatic, format!("{verb} by aterm")),
        AuthorKind::Explicit => match by.principal() {
            Some(who) => (Said::Set, format!("{verb} by {who}")),
            None => (Said::Set, verb.to_owned()),
        },
    };
    (
        said,
        format!("{verb} at {} {}", mac_of(by), on_words(by.at_ms, now_ms)),
    )
}

/// Whether a Mac's newest report of a cell disagrees with what the cell `shown` holds: a cell
/// written or holding a value ([`differs`]); a cell nothing ever wrote and that holds nothing is
/// no disagreement, only the Mac's word.
fn disagrees(shown: Option<&Shown>, seen: &Observed) -> bool {
    let Some(shown) = shown else {
        return false;
    };
    if shown.values.is_empty() {
        (shown.by.is_some() || !shown.text.is_empty()) && differs(seen, &[shown.value()])
    } else {
        let held: Vec<&str> = shown.values.iter().map(|(v, _)| v.as_str()).collect();
        differs(seen, &held)
    }
}

/// Whether the newest observation of a cell disagrees with what it holds: a Mac reports a value
/// none of its held values is (`texts`, a conflict's every value; an empty cell holds none).
fn differs(seen: &Observed, texts: &[&str]) -> bool {
    !texts
        .iter()
        .any(|text| !text.is_empty() && *text == seen.text)
}

/// A LINKED ROWS cell's words in the line under the grid ([`provenance`]): how many live rows
/// of the table titled `from` name this row, from the count the cell shows (nothing for none).
/// A conflicted cell of that table is counted once for each of its values that names the row.
/// The cell bar says the count alone, as the cell does.
fn linked_words(count: &str, from: &str) -> String {
    match count {
        "" => format!("No live row of {from} names this row"),
        "1" => format!("1 live row of {from} names this row"),
        n => format!("{n} live rows of {from} name this row"),
    }
}

/// Where the selected cell's value came from, in plain words: for what a Mac states, when it
/// saw it and which Mac; for what was written for the owner, at which Mac and when, and whether
/// a person, aterm or an import wrote it; and for a cell a Mac also states, what that Mac
/// reports beside it, whatever else the line says. Where the two disagree the line is DIFFERS and
/// says both values, neither over the other. It reads only what the page's last read holds, so it
/// changes when the page reads again and asks the registry nothing itself.
#[expect(
    clippy::too_many_lines,
    reason = "one line, every case of it in one place"
)]
pub(crate) fn provenance(
    app: &TablesApp,
    view: &TablesViewState,
    sheet: &Sheet,
    now_ms: u64,
) -> (Said, String) {
    let nothing = || {
        (
            Said::Nothing,
            "Select a cell to see where its value came from".to_owned(),
        )
    };
    let (Some(pk), Some(name)) = (&view.row, &view.column) else {
        return nothing();
    };
    let (Some(row), Some(column)) = (
        sheet.rows.iter().find(|r| r.pk == *pk),
        sheet.columns.iter().find(|c| c.name == *name),
    ) else {
        return nothing();
    };
    let field = field_words(column);
    let shown = row.cells.get(&column.name);
    let text = shown.map_or("", |c| c.text.as_str());
    if column.group == Group::Linked {
        let from = column
            .links
            .as_deref()
            .and_then(|table| app.sheet(table))
            .map_or("another table", |other| other.title);
        return (Said::Linked, linked_words(text, from));
    }
    if column.name == crate::tables_store::KEY {
        return (
            Said::Key,
            format!("{field}: {text} is this row's key, fixed when the row was added"),
        );
    }
    // What a Mac reports of the cell, said beside whatever else the line says.
    let seen = sheet.observed.get(&(pk.clone(), column.name.clone()));
    let reports = |seen: &Observed| {
        format!(
            "{} reports {} ({}), {}",
            mac_of(&seen.by),
            value_words(&seen.text),
            seen.from,
            ago_words(seen.at_ms, now_ms)
        )
    };
    let agrees = |seen: &Observed| {
        format!(
            "{} reports the same ({}), {}",
            mac_of(&seen.by),
            seen.from,
            ago_words(seen.at_ms, now_ms)
        )
    };
    if let Some(typed) = app
        .staged
        .get(&(sheet.name.to_owned(), pk.clone(), column.name.clone()))
    {
        let mut words = format!(
            "{field}: {} set here, not written; the registry holds {}",
            typed_words(column, typed),
            value_words(text)
        );
        if let Some(seen) = seen {
            words.push_str(" \u{b7} ");
            words.push_str(&reports(seen));
        }
        return (Said::Unwritten, words);
    }
    if let Some(shown) = shown.filter(|c| !c.values.is_empty()) {
        let each: Vec<String> = shown
            .values
            .iter()
            .map(|(value, by)| match by {
                Some(by) => format!(
                    "{} {}",
                    value_words(value),
                    set_words(by, value.is_empty(), now_ms).1
                ),
                None => value_words(value),
            })
            .collect();
        let mut words = format!(
            "{field}: {} values, none chosen: {}",
            shown.values.len(),
            each.join("; ")
        );
        let mut said = Said::Values;
        if let Some(seen) = seen {
            let held: Vec<&str> = shown.values.iter().map(|(v, _)| v.as_str()).collect();
            words.push_str(" \u{b7} ");
            if differs(seen, &held) {
                said = Said::Differs;
                words.push_str(&reports(seen));
            } else {
                words.push_str(&agrees(seen));
            }
        }
        return (said, words);
    }
    // The key a reference holds, where the cell shows its row's words.
    let named = shown
        .and_then(|c| c.refers.as_deref())
        .filter(|key| *key != text)
        .map_or_else(String::new, |key| format!(" ({key})"));
    let by = shown.and_then(|c| c.by.as_ref());
    // What a Mac states of itself is an observation: when it saw it, and which Mac.
    if sheet.owned
        && let Some(by) = by
    {
        let at = shown.and_then(|c| c.seen_ms).unwrap_or(by.at_ms);
        return (
            Said::Seen,
            format!(
                "{field}{named}: seen {} by {}",
                ago_words(at, now_ms),
                mac_of(by)
            ),
        );
    }
    let Some(seen) = seen else {
        return match by {
            Some(by) => {
                let (said, how) = set_words(by, text.is_empty(), now_ms);
                (said, format!("{field}{named}: {how}"))
            }
            None if text.is_empty() => (Said::Nothing, format!("{field}: empty")),
            None => (
                Said::Nothing,
                format!("{field}{named}: no writer recorded here"),
            ),
        };
    };
    // A cell a Mac also reports. Nothing was ever written here: the Mac's is the only word.
    let Some(by) = by else {
        if text.is_empty() {
            return (
                Said::Seen,
                format!("{field}: not set here; {}", reports(seen)),
            );
        }
        let held = format!("{field}{named}: {text}, no writer recorded here");
        return if differs(seen, &[text]) {
            (Said::Differs, format!("{held} \u{b7} {}", reports(seen)))
        } else {
            (Said::Nothing, format!("{held} \u{b7} {}", agrees(seen)))
        };
    };
    let (said, how) = set_words(by, text.is_empty(), now_ms);
    if differs(seen, &[text]) {
        // Both values said: the one held (or its clearing) and the one the Mac reports.
        let held = if text.is_empty() {
            how
        } else {
            format!("{text} {how}")
        };
        (
            Said::Differs,
            format!("{field}{named}: {held} \u{b7} {}", reports(seen)),
        )
    } else {
        (
            said,
            format!("{field}{named}: {how} \u{b7} {}", agrees(seen)),
        )
    }
}

/// The line under the grid that says where the selected cell's value came from
/// ([`provenance`]): its word ([`Said::tag`]) and its sentence, set as [`Said::style`] says and
/// wrapped onto as many lines as it needs, so every value it names is read whole; and how tall
/// it stands.
fn provenance_line(
    app: &TablesApp,
    view: &TablesViewState,
    sheet: &Sheet,
    width: f32,
) -> (UiNode, f32) {
    let (said, sentence) = provenance(app, view, sheet, aterm_time::unix_millis().unwrap_or(0));
    let mut children = Vec::new();
    let tag = said.tag();
    let mut tag_w = 0.0;
    if !tag.is_empty() {
        tag_w = text_width(tag, SemanticRole::Text, said.tag_style()) + 6.0;
        children.push(words(
            "tables/said/tag",
            tag.to_owned(),
            said.tag_style(),
            Length::Fixed(tag_w),
        ));
    }
    let sentence = crate::native_about::wrapped(
        "tables/said/words",
        &sentence,
        (width - tag_w - 28.0).max(1.0),
        SemanticRole::Text,
        said.style(),
    );
    let tall = match sentence.layout.height {
        Length::Fixed(h) => h,
        _ => 0.0,
    }
    .max(24.0 * scale().max(1.0));
    children.push(sentence);
    (
        group("tables/said")
            .layout(Layout::row().gap(8.0).padding(Insets::symmetric(10.0, 0.0)))
            .children(children),
        tall,
    )
}

/// How tall one line of the line under the grid ([`provenance_line`]) stands at most: in the
/// body face, the largest any text of its role is set in, whichever style [`Said::style`] picks.
fn said_line() -> f32 {
    crate::native_about::wrapped_line_height(SemanticRole::Text, StyleRef::Plain)
}

/// The width under which the page stands in one column: the rail is one table picker
/// ([`PICKER`]) and the pane sits below the grid (docs/DESIGN-tables-app-2026-11-14.md §3.6).
pub(crate) const WIDE: f32 = 900.0;

/// Under [`WIDE`]: the picker that stands for the rail, in the title's place, naming what the
/// page shows.
fn picker(app: &TablesApp, view: &TablesViewState) -> UiNode {
    let title = if view.check.open {
        "Check"
    } else {
        app.current(view)
            .and_then(|name| app.title_of(name))
            .unwrap_or(TABLES_TITLE)
    };
    let spoken = if view.picking {
        format!("Table: {title}; close the list of tables")
    } else {
        format!("Table: {title}; choose another table")
    };
    let pick = button(PICKER, title, &spoken, StyleRef::Secondary, true);
    // The mark that says the button opens a list is painted beside its name, not spoken.
    let mark = if view.picking { "\u{25b4}" } else { "\u{25be}" };
    group("tables/picker-line")
        .layout(
            Layout::row()
                .gap(6.0)
                .width(Length::Fill)
                .padding(Insets::symmetric(0.0, 3.0)),
        )
        .children(vec![
            pick,
            fitted("tables/picker/mark", mark, StyleRef::Note, 16.0).paint_only_tree(),
        ])
}

/// The pane beside Check's list, or below it: the selected finding whole.
fn check_pane(app: &TablesApp, view: &TablesViewState, width: f32, height: f32) -> UiNode {
    let mut tall = 0.0;
    let children = crate::native_tables_check::detail(app, view, width)
        .into_iter()
        .map(|(node, h)| {
            tall += h + 4.0;
            node.layout_height(h)
        })
        .collect();
    pane_frame(children, width, height, view, tall)
}

#[expect(clippy::too_many_lines, reason = "one page, laid out in one pass")]
fn page(app: &TablesApp, view: &TablesViewState, width: f32, height: f32) -> UiTree {
    let narrow = width < WIDE;
    view.narrow.set(narrow);
    let rail_w = if narrow {
        0.0
    } else {
        188.0_f32.min(width * 0.25)
    };
    let pane_w = if narrow {
        0.0
    } else {
        340.0_f32.min(width * 0.3)
    };
    let main_w = (width - rail_w - pane_w - 32.0 - if pane_w > 0.0 { 12.0 } else { 0.0 }).max(1.0);
    let line = 30.0 * scale().max(1.0);
    let head_h = 36.0 * scale().max(1.0);
    let mut main = Vec::new();
    // The room inside the main column's padding, less each child placed and its gap.
    let mut room = height - 32.0;
    // A pane below the main column's other children, where the page is narrow.
    let mut below: Option<UiNode> = None;
    // Where a column's list of values opens, down the main column: under the grid's header line,
    // or under the chips' line.
    let mut filter_at: (f32, Option<f32>) = (0.0, None);
    // The height of the line under the header that says which records the read does not
    // project, with its gap: none where it says nothing.
    let mut refused_h = 0.0;
    let current = app.current(view);
    let sheet = current.and_then(|name| app.sheet(name));
    if narrow && view.picking {
        main.push(
            group("tables/header")
                .layout(Layout::row().height(Length::Fixed(head_h)))
                .children(vec![picker(app, view)]),
        );
        room -= head_h + 10.0;
        main.push(rail(app, view, main_w, room).layout_height(room.max(line)));
    } else if view.check.open {
        main.push(crate::native_tables_check::heading(
            app,
            main_w,
            narrow.then(|| picker(app, view)),
        ));
        room -= head_h + 10.0;
        let list_h = if narrow {
            (room * 0.55).max(line * 3.0)
        } else {
            room.max(line * 3.0)
        };
        main.push(crate::native_tables_check::list(app, view, main_w, list_h));
        if narrow {
            below = Some(check_pane(
                app,
                view,
                main_w,
                (room - list_h - 10.0).max(160.0),
            ));
        }
    } else {
        let title = current
            .and_then(|name| app.title_of(name))
            .unwrap_or(TABLES_TITLE);
        let singular_count = sheet.map_or_else(String::new, |s| {
            rows_words(view.shown_count(app, s), rows::admitted(app, view, s))
        });
        let count_w =
            text_width(&singular_count, SemanticRole::Text, StyleRef::Note).min(main_w * 0.4);
        main.push(
            group("tables/header")
                .layout(Layout::row().gap(12.0).height(Length::Fixed(head_h)))
                .children(vec![
                    if narrow {
                        picker(app, view)
                    } else {
                        UiNode::new("tables/title", UiContent::Text(TextSpec::heading(title)))
                            .layout(Layout::default().width(Length::Fill))
                    },
                    fitted("tables/count", &singular_count, StyleRef::Note, count_w),
                ]),
        );
        // Records the read holds and does not project, said on a line of its own under the
        // header whenever it holds any: that line and the gap under it move everything below
        // down, the lists that open over the page included.
        for (key, words) in [
            ("tables/refused", app.refused()),
            ("tables/missing-record", view.missing_record.as_deref()),
        ] {
            if let Some(words) = words {
                main.push(fitted(key, words, StyleRef::Note, main_w).layout_height(line));
                refused_h += line + 10.0;
            }
        }
        room -= refused_h;
        main.push(tools(app, view, sheet, main_w, line));
        room -= head_h + 10.0 + line + 4.0 + 10.0;
        // The filters that stand, as chips on lines of their own under the tools: their height
        // and the gap under them come off the room once, as the tools row's do, before the cell
        // bar and the history drawer are judged on what is left.
        let mut grid_top = 16.0 + head_h + 10.0 + refused_h + line + 4.0 + 10.0;
        if let Some((chips, chips_h)) =
            sheet.and_then(|s| native_tables_filters::chips(app, view, s, main_w, line))
        {
            main.push(chips);
            room -= chips_h + 10.0;
            filter_at.1 = Some(grid_top);
            grid_top += chips_h + 10.0;
        }
        let bar = write_bar(app, main_w, sheet.is_some() && app.snapshot.is_none());
        // The write bar's height and the gap over it, and the line under the grid at its least.
        let write_h = 40.0 * scale().max(1.0) + 10.0;
        let said_least = 24.0 * scale().max(1.0);
        let bar_h = if bar.is_some() { write_h } else { 0.0 };
        let said = sheet.map(|sheet| provenance_line(app, view, sheet, main_w));
        let said_h = said.as_ref().map_or(said_least, |(_, h)| *h);
        // One budget for the grid's share: the cell bar over the grid and the history drawer's
        // line under it. Whether each stands is the window's to say (its height, its width and
        // the text scale) with the view the person set over the grid, the tools row and the
        // filters' chips while any stands (a filter is the view's, as the find is, never a
        // selection), and never the selection's or a staged change's: both are judged on the
        // room the grid would have under the tools and the chips, a write bar and three lines of
        // the line under the grid, whether or not either is there. So a filter added in a short
        // window can take the bar, then the drawer, away, and taking it off brings them back. The
        // drawer's line is the one way to the table's records
        // and As of, so it is taken out of that room first: the drawer stands where what is left
        // holds three lines of grid. The bar is judged on what the drawer's line leaves, and on a
        // narrow page on half of that, as the grid stands with a row selected: it stands where
        // that holds the bar and, under it, the grid's own lines ([`grid_chrome`]) and one whole
        // row, so the row the grid keeps in view shows whole. So the bar stands only where the
        // drawer does, and in a window too short for both the bar gives way first. Typing into a
        // cell, or selecting one whose line wraps, takes neither away nor brings either back, and
        // while the line wraps onto three lines or fewer the grid under the bar keeps its whole
        // row and the write bar ends inside the window.
        let cell_line = row_height();
        let under_bar = grid_chrome() + row_height();
        let said_judged = (3.0 * said_line()).max(said_least);
        let steady = room - write_h - said_judged - 10.0;
        let drawer_least = if sheet.is_some() {
            history::least()
        } else {
            0.0
        };
        let drawn = sheet.is_some() && steady - drawer_least >= line * 3.0;
        let barred =
            (if narrow { steady * 0.5 } else { steady }) - drawer_least >= cell_line + under_bar;
        room -= bar_h + said_h + 10.0;
        let drawer_room = if drawn { drawer_least } else { 0.0 };
        // Narrow, with a row selected, the grid takes half what is left and the row's pane the
        // rest below it; with none, the grid takes it all. The share keeps the work's least (under
        // the bar the grid's own lines and a whole row, else three lines of grid) and the
        // drawer's line, as far as the room holds them: a line wrapped onto more than three lines
        // takes its room from the grid's, down to three lines that the grid, its bar and the
        // drawer's line keep between them, so neither the bar nor the drawer pushes the write bar
        // past the window's foot.
        let below_row = narrow && view.row.is_some();
        let keep = if barred {
            cell_line + under_bar
        } else {
            line * 3.0
        } + drawer_room;
        let floor = keep.min(room.max(line * 3.0));
        let grid_h = if below_row { room * 0.5 } else { room }.max(floor);
        // The cell bar stands on the grid, a row high, at every width, in the room worked out
        // above for the grid: the grid gives the bar that row, once, so the line under it and
        // a narrow page's pane stand where they would without the bar. In a window too short
        // for the bar over the grid's lines and a row the bar gives way and the grid keeps the
        // room. The history drawer under the line takes its height out of the same share, once,
        // and never what the work's floor keeps ([`history::heights`]): the line, the write bar
        // and a narrow page's pane stand where they would without it, but where half the room is
        // less than the work's least and the drawer's line, which the share then rises to.
        let bar_row = if barred { cell_line } else { 0.0 };
        let (work_h, drawer_h) = if drawn {
            history::heights(
                grid_h,
                bar_row,
                floor - drawer_room,
                height - 32.0,
                view.history.open,
            )
        } else {
            (grid_h, 0.0)
        };
        view.history.drawn.set(drawn);
        let sheet_h = work_h - bar_row;
        filter_at.0 = grid_top + bar_row + 1.0 + head_line() + row_height();
        match (sheet, &app.unread) {
            (Some(sheet), _) => {
                let mut work = Vec::with_capacity(2);
                if barred {
                    work.push(cell_bar(app, view, sheet, main_w).layout_height(cell_line));
                }
                work.push(grid(app, view, sheet, main_w, sheet_h).layout_height(sheet_h));
                main.push(
                    group("tables/work")
                        .layout(Layout::column().height(Length::Fixed(work_h)))
                        .children(work),
                );
                if let Some((line, h)) = said {
                    main.push(line.layout_height(h));
                }
                if drawn {
                    main.push(history::drawer(app, view, sheet, main_w, drawer_h));
                }
                if below_row {
                    below = Some(pane(
                        app,
                        view,
                        sheet,
                        main_w,
                        (room - grid_h - 10.0).max(160.0),
                    ));
                }
            }
            (None, Some(why)) => main.push(crate::native_about::wrapped(
                "tables/unread",
                &format!("The tables could not be read here: {why}"),
                main_w,
                SemanticRole::Text,
                StyleRef::Note,
            )),
            (None, None) => main.push(
                fitted("tables/reading", "Reading\u{2026}", StyleRef::Note, main_w)
                    .layout_height(line),
            ),
        }
        if let Some(bar) = bar {
            main.push(bar);
        }
    }
    if let Some(below) = below {
        main.push(below);
    }
    let mut columns = Vec::new();
    if !narrow {
        columns.push(rail(app, view, rail_w, height));
    }
    // `main_w` is the room inside the column's padding.
    let main = group("tables/main")
        .layout(
            Layout::column()
                .gap(10.0)
                .width(Length::Fixed(main_w + 32.0))
                .padding(Insets::all(16.0)),
        )
        .children(main);
    // The open list of columns, over the main column under the tools row's Columns button.
    let list = sheet
        .filter(|_| view.columns.open && !(narrow && view.picking) && !view.check.open)
        .map(|sheet| {
            let under = 16.0 + head_h + 10.0 + refused_h + line + 4.0 + 2.0;
            native_tables_columns::list(app, view, sheet, (16.0 + main_w, under, height - 16.0))
        });
    // The open list of a column's values, under its mark or its chip.
    let values = sheet
        .filter(|_| !(narrow && view.picking) && !view.check.open)
        .and_then(|sheet| {
            native_tables_filters::list(
                app,
                view,
                sheet,
                (16.0, 16.0 + main_w, 16.0),
                (filter_at.0, filter_at.1, height - 16.0),
            )
        });
    columns.push(match list.or(values) {
        Some(list) => native_tables_columns::over(main, list, main_w + 32.0),
        None => main,
    });
    // Narrow, the pane is below and laid out there: laying it out again here would set its
    // scroll's limit and its keys for a pane not drawn.
    let beside = if narrow {
        None
    } else if view.check.open {
        Some(check_pane(app, view, pane_w, height - 32.0))
    } else {
        sheet.map(|sheet| pane(app, view, sheet, pane_w, height - 32.0))
    };
    if let Some(beside) = beside {
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
                .children(vec![beside]),
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

/// The line of tools over the grid: the find; for a table with a lifecycle, which rows it shows
/// by it and, as it is now, the way to add one or who adds one; in a past table which point it
/// is, how much changed since, and the way back to now; and at its right end the Columns button.
///
/// Every tool has a width of its own and none runs past the row or over the next one, at any
/// width and text size. Where the row is short of room it is given up in this order: the find,
/// down to a width a find is still typed in at ease; the Columns button's count (it paints
/// `Columns` alone where anything more would be given up beside the count); what stands between
/// the lifecycle filter and the button (the Add, who adds a row, the words naming a past point),
/// cut; the rest of the find; then the filter, Now and the Columns button themselves, whose
/// labels are then cut.
fn tools(
    app: &TablesApp,
    view: &TablesViewState,
    sheet: Option<&Sheet>,
    main_w: f32,
    line: f32,
) -> UiNode {
    const GAP: f32 = 12.0;
    let past = app.as_of();
    let now_ms = aterm_time::unix_millis().unwrap_or(0);
    // A past point's words: who wrote it, when, and how many of the cells the past grid shows
    // changed since (a row born since has no line there).
    let words = past.map(|at| {
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
        words
    });
    // What each tool takes whole: what stands between the filter and the Columns button, the
    // find, the lifecycle filter (a past point's too, since it picks that point's rows as well),
    // Now, and the Columns button.
    let between = match &words {
        Some(words) => Some(text_width(words, SemanticRole::Text, StyleRef::Plain)),
        None => sheet.and_then(|s| rows::add_width(app, s)),
    };
    let find = (220.0 * scale().max(1.0)).min(main_w * 0.5);
    let filter = sheet.and_then(|s| rows::filter_width(view, s));
    let now = past.map(|_| crate::native_ui::native_button_label_width("Now") + 24.0);
    let wants = |short: bool| {
        [
            between,
            Some(find),
            filter,
            now,
            sheet.map(|s| native_tables_columns::control_width(app, view, s, short)),
        ]
    };
    // The order the row gives up room in, each step taking it from one tool down to a width: the
    // find to the width it still finds in at ease, what stands between, the find to a point,
    // then the filter, Now and the Columns button.
    let steps = [
        (1, (120.0 * scale().max(1.0)).min(find)),
        (0, 1.0),
        (1, 1.0),
        (2, 1.0),
        (3, 1.0),
        (4, 1.0),
    ];
    // The gaps between the tools: the Columns button has a space of its own before it, which
    // fills what the others leave.
    let parts = wants(false).iter().flatten().count() + usize::from(sheet.is_some());
    let room = main_w - GAP * parts.saturating_sub(1) as f32;
    // The Columns button's count goes where more than the find's ease would be given up beside it.
    let (mut widths, took) = give_up(room, wants(false), &steps);
    let short = sheet.is_some() && took.is_some_and(|step| step > 0);
    if short {
        widths = give_up(room, wants(true), &steps).0;
    }
    let [between_w, find_w, filter_w, now_w, control_w] = widths;
    let mut tools = vec![
        field(FIND, "Find", &view.find, view)
            .layout(Layout::default().width(Length::Fixed(find_w.unwrap_or(find)))),
    ];
    if let (Some(sheet), Some(w)) = (sheet, filter_w)
        && let Some(filter) = rows::filter_tool(view, sheet, w)
    {
        tools.push(filter);
    }
    match (&words, sheet, between_w) {
        (Some(words), _, Some(w)) => tools.push(fitted("tables/asof", words, StyleRef::Plain, w)),
        // The table as it is: the way to add a row, or who adds one.
        (None, Some(sheet), Some(w)) => tools.extend(rows::add_tool(app, sheet, w)),
        _ => {}
    }
    if let Some(w) = now_w {
        let mut now = button(
            NOW,
            "Now",
            "Show the table as it is now",
            StyleRef::Secondary,
            true,
        );
        now.layout.width = Length::Fixed(w);
        tools.push(
            group("tables/now-seat")
                .layout(
                    Layout::row()
                        .width(Length::Fixed(w))
                        .padding(Insets::symmetric(0.0, 3.0)),
                )
                .children(vec![now]),
        );
    }
    if let (Some(sheet), Some(w)) = (sheet, control_w) {
        tools.push(group("tables/tools/lead").layout(Layout::default().width(Length::Fill)));
        tools.push(native_tables_columns::control(app, view, sheet, short, w));
    }
    group("tables/tools")
        .layout(Layout::row().gap(GAP).height(Length::Fixed(line + 4.0)))
        .children(tools)
}

/// The widths of tools that each want `wants` (`None`: not on the row), fitted to `room`: each
/// of `steps` in turn takes room from one tool (by its place in `wants`) down to a width, until
/// they fit or the steps run out; and the last step that took any.
fn give_up<const N: usize>(
    room: f32,
    wants: [Option<f32>; N],
    steps: &[(usize, f32)],
) -> ([Option<f32>; N], Option<usize>) {
    let mut widths = wants;
    let mut over = widths.iter().flatten().sum::<f32>() - room;
    let mut took = None;
    for (step, (at, least)) in steps.iter().enumerate() {
        if over <= 0.0 {
            break;
        }
        if let Some(w) = widths.get_mut(*at).and_then(Option::as_mut) {
            let gives = (*w - least).max(0.0).min(over);
            if gives > 0.0 {
                *w -= gives;
                over -= gives;
                took = Some(step);
            }
        }
    }
    (widths, took)
}

pub(crate) trait FixedHeight {
    fn layout_height(self, h: f32) -> Self;
}

impl FixedHeight for UiNode {
    fn layout_height(mut self, h: f32) -> Self {
        self.layout.height = Length::Fixed(h);
        self
    }
}

#[path = "native_tables_rows.rs"]
pub(crate) mod rows;

#[cfg(all(test, unix))]
#[path = "native_tables_tests.rs"]
pub(crate) mod native_tables_tests;

#[cfg(all(test, unix))]
#[path = "native_tables_narrow_tests.rs"]
mod native_tables_narrow_tests;

#[cfg(all(test, unix))]
#[path = "native_tables_types_tests.rs"]
mod tests;
