// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Tables page's history drawer (tracker package LT-39): under the grid, every cell a record
//! set in the shown table, at one of three scopes: the selected cell, the selected row, or the
//! whole table. It is a view over what the page already read ([`Sheet::written`]): it makes no
//! read of its own and writes nothing.
//!
//! - **Closed,** as the page opens, it is one line: the newest record of the table (who, where
//!   the history places it, how many cells it set) and Show history.
//! - **Open,** it is a head (the scope, how many records and cells it lists, Hide history) over
//!   a list that scrolls by itself, a bar down its right edge where the list is longer than its
//!   room. It takes its height from the grid's ([`heights`]); nothing else on the page moves.
//!   In a head too narrow for all of it the counts give way first, then the title, then the
//!   scope's words; Hide history stays whole.
//! - **In a short window.** Whether it stands is the window's to say, as whether the cell bar
//!   does: judged on the room the grid would have under the tools row, the filters' chips while
//!   any stands, a write bar and three lines of the line under the grid, so no selection and no
//!   staged change takes it away or brings it back. A filter is the view the person set, as the
//!   tools are, so one added in a short window can take the drawer away, and taking it off
//!   brings the drawer back (the keys are on the list of values then, never on the drawer). It
//!   is the one way to a table's records and As of, so its line comes out of that room first
//!   and in a window too short for its line, the cell bar and the grid under the bar the cell
//!   bar gives way first ([`least`]); it gives way, with the pane's way to it, where that room
//!   leaves three lines of grid less than its line, on a narrow page as on a wide one (the bar
//!   alone is judged on half of it there). It never runs the page past the window's foot or
//!   stands cut short: a line under the grid wrapped onto more than three lines takes its room
//!   from the grid, down to three lines that the grid, its bar and the drawer's line keep
//!   between them, and open, its list's room is a whole number of lines, at least one, that a
//!   scroll moves by whole lines, so no line shows only in part; in a share whose room over the
//!   work's least holds no whole line it is its head alone ([`heights`] splits what the page
//!   gives it).
//! - **Filters and the find** narrow the grid's rows, never the history: every scope lists what
//!   it lists whatever they show (Table scope every record of the table, the closed line its
//!   newest), and the closed line says nothing of them. A column's list of values opens over
//!   the drawer as over the grid ([`UiNode::surface`]): a press or the wheel over it never
//!   reaches the drawer's lines, and Escape closes the list first.
//! - **Order.** Lines run newest first in the order [`Sheet::written`] holds, which is the
//!   registry's causal order; nothing here sorts by a claimed time. A day head says the day the
//!   record under it claims, where that differs from the record before.
//! - **A line** is one cell a record set: the row's key text and the column's words, then the
//!   value written as the grid shows it (`empty` for a cleared cell, `+ x` and `− x` for a set's
//!   member put in or taken out, `Row → added` for a row's birth, the first `live` of its
//!   lifecycle). A record that set one cell in the scope is one line with its writer and time;
//!   one that set several is a head with its writer, time and counts over a line for each cell,
//!   folded where there are more than [`FOLD_OVER`]. A retire, an end or a restore prints its
//!   lifecycle's value where the grid shows that column, as it does on every table whose rows
//!   the page retires (LT-24); the import columns print no line, since the page shows none.
//! - **No earlier value.** A line says what was written and never `was → now`. The line before
//!   it in this list is not what a write replaced: two machines that wrote one cell without
//!   seeing each other are both listed, and neither replaced the other. The store read that says
//!   what a write replaced is tracker package LT-34; until the page has it, the list at Cell
//!   scope is itself the sequence of values written. A birth's `Row → added` is the page's word
//!   for a new row, not an earlier value.
//! - **As of.** Each record's line offers the table as that record's causal past holds it, as
//!   the pane's history did; the record the page shows as of is set as the page's current
//!   choice. The current scope is marked as the rail marks the table shown: a neutral band under
//!   words in the primary ink, never the accent.
//! - **Not written.** The staged cells the scope holds come first, under `NOT WRITTEN`, in the
//!   page's one blue ([`StyleRef::Unwritten`]), each with Undo; one the table's rules refuse
//!   says `refused` in the danger ink, as its grid cell does; a reference staged reads as the
//!   row it names, as its cell does (LT-30). A row's retire, end or restore is its lifecycle
//!   cell staged and lists as one, its Undo the pane's under the fields. A new row added here
//!   leads with its birth, `Row → added` at its lifecycle's cell, named `New <row>` as its pane
//!   and the cell bar name it (as are the cells staged in it, which follow), so a new row with
//!   no cell staged yet has its line too, and says `refused` where its lifecycle cell does (a
//!   key the table holds already); that line's Undo takes the row back and every cell staged in
//!   it, as the pane's Remove does, and is off while a Write is out.
//! - **Selection, both ways.** A press on a line selects its cell in the grid, as a press on the
//!   cell does, and opens nothing. A row the filters on the columns' values alone leave out
//!   shows, by the rule Check's Show row follows: they are cleared
//!   ([`TablesViewState::clear_filters_for`]). A row the grid does not show otherwise (Find or
//!   the lifecycle filter narrowed it away, or it is no longer live) is named in the head as its
//!   lines name it (a new row as `New <row>`), and the filters and the selection stay. A line
//!   names a cell, and seeing it is what the press asks: the press shows the cell's column as its
//!   line in the Columns list does (hidden no longer, and out of the fold where no row fills it)
//!   and the grid scrolls across to it, so no press selects a cell the grid does not draw. A cell
//!   selected in the grid narrows the Row and Cell scopes to it, a selection the columns moved
//!   (its column hidden from the list) as any other; at Table scope its newest line wears the
//!   grid's neutral ring ([`StyleRef::CellSelected`]), as the grid's cell does, and comes into
//!   view.
//!   A read that brings records puts their lines above the ones shown, which stay where they
//!   stand on the screen; one whose record set the selected cell brings the ring's new line
//!   into view.
//! - **Keys.** The list is one Tab stop, so Tab goes from the grid through the drawer's head
//!   and its list to the pane whatever the table holds: Up and Down move from line to line,
//!   Left and Right on to a line's As of or Undo. After a pointer's press the keys are the
//!   selected cell's again (an open cell's, while one is open), as after a press on a header;
//!   pressed from the keyboard, a control keeps them, and an Undo that goes with its line
//!   leaves them on Hide history, as does a line that goes under them for no press of theirs
//!   (a Write's answer, the Columns moving the selected cell).
//! - **Secrets.** A line prints [`super::WrittenCell::now`], which holds for a secret cell what the
//!   grid shows (its store entry's name, or the mask) and nothing else.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};

use super::{
    AS_OF, At, Author, Sheet, TablesApp, TablesViewState, Written, button, field_words, fitted,
    grid_chrome, group, press, row_height, rows, scale, styled, value_words, writer,
};
use crate::native_ui::{
    ActionId, ButtonSpec, Control, ControlState, GroupSpec, Insets, Layout, Length, SemanticRole,
    SemanticValue, StyleRef, UiContent, UiNode, native_button_label_width,
};
use crate::native_workers::text_width;

/// The drawer, and the start of every key under it: a wheel over any of them scrolls its list.
pub(super) const KEY: &str = "tables/history";
const SHOW: &str = "tables/history/show";
const HIDE: &str = "tables/history/hide";
/// A scope's segment: `cell`, `row` or `table` follows.
const SCOPE: &str = "tables/history/scope/";
/// A cell's line: the record's digest and the cell's place among those it set follow.
const LINE: &str = "tables/history/at/";
/// A folded record's head: the record's digest follows.
const FOLD: &str = "tables/history/fold/";
/// A staged cell's line: its row's key and its column follow.
const STAGED: &str = "tables/history/staged/";
/// Undo of a staged cell: its row's key and its column follow.
const UNDO: &str = "tables/history/undo/";
/// The pane's button that opens the drawer on the selected row.
pub(super) const OF_ROW: &str = "tables/pane/history";
/// A record that set more cells than this in the scope starts folded.
pub(super) const FOLD_OVER: usize = 8;
/// The room the main column keeps between the grid and the drawer.
const GAP: f32 = 10.0;
/// The room the open drawer keeps under its list.
const FOOT: f32 = 6.0;
/// How far a cell's line is set in under its record's head.
const INDENT: f32 = 16.0;

/// What the drawer lists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Scope {
    /// Every value written to the selected cell.
    Cell,
    /// Every write to the selected row.
    Row,
    /// Every write to the shown table.
    #[default]
    Table,
}

impl Scope {
    const ALL: [Self; 3] = [Self::Cell, Self::Row, Self::Table];

    const fn word(self) -> &'static str {
        match self {
            Self::Cell => "Cell",
            Self::Row => "Row",
            Self::Table => "Table",
        }
    }

    const fn slug(self) -> &'static str {
        match self {
            Self::Cell => "cell",
            Self::Row => "row",
            Self::Table => "table",
        }
    }

    const fn spoken(self) -> &'static str {
        match self {
            Self::Cell => "History of the selected cell",
            Self::Row => "History of the selected row",
            Self::Table => "History of this table",
        }
    }
}

/// The grid's selection as the drawer reads it: the row's key, and the column where one is
/// selected.
pub(crate) type Selection<'a> = Option<(&'a str, Option<&'a str>)>;

/// One cell a record set, as its line says it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    /// Its place among the cells its record set ([`Written::cells`]).
    pub(crate) index: usize,
    pub(crate) pk: String,
    /// The grid column a press on the line selects: the cell's own; for a birth, the
    /// lifecycle's where the grid shows it, else the row's key column.
    pub(crate) column: String,
    /// The line is a row's birth (`Row → added`), not a value as the grid shows it.
    pub(crate) born: bool,
    /// `<row's key text> · <column words>`; a birth names the row alone.
    pub(crate) place: String,
    /// The value written, as the grid shows it: `empty` for a cleared cell, a set's member as
    /// the store words it, `Row → added` for a birth.
    pub(crate) value: String,
    /// The write cleared the cell: `value` is the word `empty`, set faint.
    pub(crate) cleared: bool,
    /// At Table scope, the newest line of the cell the grid has selected.
    pub(crate) ringed: bool,
}

/// One line of the open drawer's list, top down.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Line {
    /// `NOT WRITTEN`, over the staged cells.
    NotWrittenHead,
    /// A staged cell: where it is and the words staged in it.
    NotWritten {
        pk: String,
        column: String,
        place: String,
        text: String,
    },
    /// The day the record under it claims.
    Day(String),
    /// A record that set one cell in the scope.
    One {
        operation: [u8; 32],
        by: Author,
        /// The page shows the table as of this record.
        here: bool,
        cell: Entry,
    },
    /// A record that set several cells in the scope: its lines follow, unless it is folded.
    Head {
        operation: [u8; 32],
        by: Author,
        here: bool,
        cells: usize,
        rows: usize,
        /// Whether it folds (it set more than [`FOLD_OVER`] cells here), and is open.
        open: Option<bool>,
        /// Folded over the newest line of the cell the grid has selected.
        ringed: bool,
    },
    /// One cell of the record whose [`Line::Head`] stands over it, `by` its writer.
    Cell {
        operation: [u8; 32],
        by: Author,
        cell: Entry,
    },
    /// Row or Cell scope with nothing selected.
    Unselected,
    /// The last line: nothing older was read.
    Start,
}

impl Line {
    /// Whether the line wears the selected cell's ring.
    const fn ringed(&self) -> bool {
        match self {
            Self::One { cell, .. } | Self::Cell { cell, .. } => cell.ringed,
            Self::Head { ringed, .. } => *ringed,
            _ => false,
        }
    }
}

/// How much the drawer lists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Tally {
    /// The table's records that print a line at Table scope.
    pub(crate) all: usize,
    /// Those the scope lists.
    pub(crate) records: usize,
    /// The cells they set in the scope.
    pub(crate) cells: usize,
}

impl Tally {
    /// How many records and cells the scope lists: `7 records · 23 cells set`, and
    /// `4 of 7 records` where the scope narrows them.
    fn count(self, scope: Scope) -> String {
        let records = match self.all {
            0 => return "No records".to_owned(),
            1 => "1 record".to_owned(),
            n => format!("{n} records"),
        };
        let records = if scope == Scope::Table {
            records
        } else {
            format!("{} of {records}", self.records)
        };
        let cells = match self.cells {
            1 => "1 cell set".to_owned(),
            n => format!("{n} cells set"),
        };
        format!("{records} \u{b7} {cells}")
    }

    /// The head's words: [`Self::count`], then what the list's order and its times are.
    pub(crate) fn words(self, scope: Scope) -> String {
        let count = self.count(scope);
        if self.all == 0 {
            return count;
        }
        format!("{count} \u{b7} {ORDER}")
    }
}

/// What the open drawer's head says of its list's order and of the times in it.
/// A writer is a machine, whichever system it runs (decision TK-D21).
const ORDER: &str = "in the registry's order; a time is the writing machine's claim";

/// `1 cell`, `n cells`.
fn cells_words(n: usize) -> String {
    match n {
        1 => "1 cell".to_owned(),
        n => format!("{n} cells"),
    }
}

/// Whether the scope holds the cell at `pk`, `column` (no column for a row's birth).
fn holds(scope: Scope, selection: Selection<'_>, pk: &str, column: Option<&str>) -> bool {
    match (scope, selection) {
        (Scope::Table, _) => true,
        (Scope::Row, Some((row, _))) => row == pk,
        (Scope::Cell, Some((row, Some(of)))) => row == pk && column == Some(of),
        _ => false,
    }
}

/// Whether a staged line at `pk`, `column` is a new row's birth: no cell stages a new row's
/// lifecycle, so its line stands there, and its Undo takes the row back, as its pane's Remove
/// does.
pub(crate) fn born(pk: &str, column: &str) -> bool {
    rows::is_new(pk) && column == aterm_db::typed::LIFECYCLE
}

/// Whether the scope has what it lists by: a row for Row, a cell for Cell.
const fn selected(scope: Scope, selection: Selection<'_>) -> bool {
    match (scope, selection) {
        (Scope::Table, _) | (Scope::Row, Some(_)) | (Scope::Cell, Some((_, Some(_)))) => true,
        _ => false,
    }
}

/// Each row's key text: what its key column shows in this read, its key where that is empty.
/// A row the read does not list is named by the last key text its records wrote.
fn names(sheet: &Sheet) -> BTreeMap<&str, &str> {
    let mut names = BTreeMap::new();
    for cell in sheet.written.iter().flat_map(|written| &written.cells) {
        if cell.column == sheet.key && !cell.now.is_empty() {
            names.insert(cell.pk.as_str(), cell.now.as_str());
        }
    }
    for row in &sheet.rows {
        match row.cells.get(&sheet.key).map(|cell| cell.text.as_str()) {
            Some(text) if !text.is_empty() => names.insert(row.pk.as_str(), text),
            _ => names.remove(row.pk.as_str()),
        };
    }
    names
}

/// What the drawer calls row `pk`, of the key texts `names` ([`names`]): a row added here has
/// no key a person knows, so it is the new row, as its pane and the cell bar name it, never its
/// page key; any other row is its key text.
fn row_words(sheet: &Sheet, names: &BTreeMap<&str, &str>, pk: &str) -> String {
    if rows::is_new(pk) {
        format!("New {}", sheet.singular.to_lowercase())
    } else {
        names.get(pk).copied().unwrap_or(pk).to_owned()
    }
}

/// A record and the cells of it the scope lists.
struct Listed<'a> {
    written: &'a Written,
    cells: Vec<Entry>,
}

/// The table's records that print a line in the scope, oldest first as the read holds them,
/// and how many print one at Table scope.
///
/// A row's birth prints one line, `Row → added`: the first value its lifecycle column was set
/// to, where that is `live`, whether or not the grid shows that column. Every other cell prints
/// a line where the grid shows its column: a retire, an end or a restore where it shows the
/// lifecycle (a table whose rows the page retires does), never an import column.
fn listed<'a>(
    sheet: &'a Sheet,
    scope: Scope,
    selection: Selection<'_>,
) -> (usize, Vec<Listed<'a>>) {
    let names = names(sheet);
    let key_text = |pk: &'a str| names.get(pk).copied().unwrap_or(pk);
    // The rows whose lifecycle a record set already, oldest first.
    let mut lived: BTreeSet<&str> = BTreeSet::new();
    let mut all = 0;
    let mut out = Vec::new();
    for written in &sheet.written {
        let mut prints = false;
        let mut cells = Vec::new();
        for (index, cell) in written.cells.iter().enumerate() {
            let column = sheet.columns.iter().find(|c| c.name == cell.column);
            let birth = cell.column == aterm_db::typed::LIFECYCLE
                && lived.insert(cell.pk.as_str())
                && cell.now == aterm_db::typed::grid::LIVE;
            if column.is_none() && !birth {
                continue;
            }
            prints = true;
            if !holds(scope, selection, &cell.pk, column.map(|c| c.name.as_str())) {
                continue;
            }
            cells.push(match column {
                Some(column) if !birth => Entry {
                    index,
                    pk: cell.pk.clone(),
                    column: column.name.clone(),
                    born: false,
                    place: format!("{} \u{b7} {}", key_text(&cell.pk), field_words(column)),
                    value: value_words(&cell.now),
                    cleared: cell.now.is_empty(),
                    ringed: false,
                },
                // A birth stands at the lifecycle's cell where the grid shows it (its first
                // value), else at the row's key.
                _ => Entry {
                    index,
                    pk: cell.pk.clone(),
                    column: column.map_or_else(|| sheet.key.clone(), |c| c.name.clone()),
                    born: true,
                    place: key_text(&cell.pk).to_owned(),
                    value: "Row \u{2192} added".to_owned(),
                    cleared: false,
                    ringed: false,
                },
            });
        }
        all += usize::from(prints);
        if !cells.is_empty() {
            out.push(Listed { written, cells });
        }
    }
    (all, out)
}

/// The day `at_ms` falls on by this Mac's clock, as a group head writes it: `SAT 7 NOV 2026`,
/// the day a line's claimed time ([`super::when_words`]) names.
fn day_words(at_ms: u64) -> String {
    let (days, _) = super::local_day(at_ms);
    let (year, month, day) = aterm_types::rfc3339::civil_from_days(days);
    const MONTHS: [&str; 12] = [
        "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
    ];
    const WEEKDAYS: [&str; 7] = ["THU", "FRI", "SAT", "SUN", "MON", "TUE", "WED"];
    format!(
        "{} {day} {} {year}",
        WEEKDAYS[days.rem_euclid(7) as usize],
        MONTHS[(month - 1) as usize],
    )
}

/// The open drawer's list, top down, and its counts: the staged cells the scope holds, then
/// each record that set a cell in it, newest first in the order the read holds them, each under
/// the day it claims where that differs from the record before.
///
/// `folds` says which records a person opened or closed, by their digest; `as_of` is the
/// record the page shows the table as of.
pub(crate) fn lines(
    sheet: &Sheet,
    scope: Scope,
    selection: Selection<'_>,
    staged: &BTreeMap<At, String>,
    folds: &BTreeMap<[u8; 32], bool>,
    as_of: Option<[u8; 32]>,
) -> (Tally, Vec<Line>) {
    let (all, listed) = listed(sheet, scope, selection);
    let tally = Tally {
        all,
        records: listed.len(),
        cells: listed.iter().map(|record| record.cells.len()).sum(),
    };
    if !selected(scope, selection) {
        return (tally, vec![Line::Unselected]);
    }
    let mut out = Vec::new();
    let names = names(sheet);
    let named = |pk: &str| row_words(sheet, &names, pk);
    let mut cells: Vec<(&str, Line)> = staged
        .iter()
        .filter(|((table, pk, column), _)| {
            table == sheet.name && holds(scope, selection, pk, Some(column.as_str()))
        })
        .map(|((_, pk, column), text)| {
            let words = sheet
                .columns
                .iter()
                .find(|c| c.name == *column)
                .map_or_else(|| column.clone(), field_words);
            let line = Line::NotWritten {
                pk: pk.clone(),
                column: column.clone(),
                place: format!("{} \u{b7} {words}", named(pk)),
                text: value_words(text),
            };
            (pk.as_str(), line)
        })
        .collect();
    // Each new row first, its birth (at its lifecycle's cell, which no cell stages: [`born`])
    // over the cells staged in it, then the cells staged in rows the table holds. A retire, an
    // end or a restore is its row's lifecycle cell staged, and lists as one.
    let mut unwritten = Vec::new();
    for row in sheet.rows.iter().filter(|row| rows::is_new(&row.pk)) {
        if holds(scope, selection, &row.pk, Some(aterm_db::typed::LIFECYCLE)) {
            unwritten.push(Line::NotWritten {
                pk: row.pk.clone(),
                column: aterm_db::typed::LIFECYCLE.to_owned(),
                place: named(&row.pk),
                text: "Row \u{2192} added".to_owned(),
            });
        }
        let (of_row, rest): (Vec<_>, Vec<_>) = cells.into_iter().partition(|(pk, _)| *pk == row.pk);
        unwritten.extend(of_row.into_iter().map(|(_, line)| line));
        cells = rest;
    }
    unwritten.extend(cells.into_iter().map(|(_, line)| line));
    if !unwritten.is_empty() {
        out.push(Line::NotWrittenHead);
        out.extend(unwritten);
    }
    // The newest line of the selected cell: the ring's, at Table scope.
    let ring = match (scope, selection) {
        (Scope::Table, Some((row, Some(column)))) => listed.iter().rev().find_map(|record| {
            record
                .cells
                .iter()
                .find(|cell| {
                    // A birth is a value of the lifecycle's cell, never of the key's.
                    (!cell.born || cell.column == aterm_db::typed::LIFECYCLE)
                        && cell.pk == row
                        && cell.column == column
                })
                .map(|cell| (record.written.operation, cell.index))
        }),
        _ => None,
    };
    let mut day: Option<String> = None;
    for Listed { written, mut cells } in listed.into_iter().rev() {
        let claimed = day_words(written.by.at_ms);
        if day.as_ref() != Some(&claimed) {
            out.push(Line::Day(claimed.clone()));
            day = Some(claimed);
        }
        let operation = written.operation;
        let here = as_of == Some(operation);
        if let Some((of, index)) = ring
            && of == operation
            && let Some(cell) = cells.iter_mut().find(|cell| cell.index == index)
        {
            cell.ringed = true;
        }
        if let [_] = cells.as_slice() {
            let cell = cells.remove(0);
            out.push(Line::One {
                operation,
                by: written.by.clone(),
                here,
                cell,
            });
            continue;
        }
        let rows = cells
            .iter()
            .map(|cell| cell.pk.as_str())
            .collect::<BTreeSet<_>>()
            .len();
        let open =
            (cells.len() > FOLD_OVER).then(|| folds.get(&operation).copied().unwrap_or(false));
        let folded = open == Some(false);
        out.push(Line::Head {
            operation,
            by: written.by.clone(),
            here,
            cells: cells.len(),
            rows,
            open,
            ringed: folded && cells.iter().any(|cell| cell.ringed),
        });
        if !folded {
            out.extend(cells.into_iter().map(|cell| Line::Cell {
                operation,
                by: written.by.clone(),
                cell,
            }));
        }
    }
    out.push(Line::Start);
    (tally, out)
}

/// The closed drawer's words: the newest record that set a cell the grid shows, by who, where
/// the history places it and how many cells; `No records` of a table with none.
pub(crate) fn newest_words(sheet: &Sheet, now: u64) -> String {
    let (_, listed) = listed(sheet, Scope::Table, None);
    listed.last().map_or_else(
        || "No records".to_owned(),
        |record| {
            format!(
                "{} \u{b7} {} \u{b7} {}",
                record.written.by.who(),
                record.written.by.placed(now),
                cells_words(record.cells.len())
            )
        },
    )
}

/// How tall one line of the drawer's list is.
fn line_height() -> f32 {
    26.0 * scale().max(1.0)
}

/// How tall the grid is with `rows` rows under its own lines ([`grid_chrome`]: its frame, its
/// group line, its header line and its type line).
fn grid_height(rows: f32) -> f32 {
    grid_chrome() + row_height() * rows
}

/// How tall the open drawer is with `lines` lines under its head.
fn drawer_height(lines: f32) -> f32 {
    row_height() + lines * line_height() + FOOT
}

/// How tall the grid's work (the cell bar, `bar` tall where it stands, over the grid) and the
/// drawer are, in the `share` points the page gave the work, with the room between them:
/// (work, drawer); the two take the share and no more.
///
/// Closed, the drawer is one line of the grid's row height. Open, it takes two fifths of the
/// main column's height (`column`), and at least five lines, while that leaves the grid six
/// rows; in a share too small for both, the grid keeps four rows and the drawer has the rest.
/// Its list's room is a whole number of lines, at least one: what it would have is rounded up
/// to its next whole line where the grid can give that and keep what it keeps (six rows where
/// the share holds both, the page's `floor` for the work where not), else down. Every scroll
/// of the list moves it by whole lines, so a line shows whole or not at all, and in a share
/// whose room over the floor holds no whole line the drawer is its head alone: never a list
/// that shows a part of a line and no line whole. Whether the drawer stands is not decided
/// here: the page asks only where it does, judged from the window and the filters' chips while
/// any stands, and gives it a share that holds the floor and the drawer's line ([`least`]).
pub(super) fn heights(share: f32, bar: f32, floor: f32, column: f32, open: bool) -> (f32, f32) {
    let room = share - GAP;
    if !open {
        return (room - row_height(), row_height());
    }
    let work = |rows: f32| bar + grid_height(rows);
    // What the drawer would have, and the most it may take.
    let (would, most) = if room >= work(6.0) + drawer_height(5.0) {
        (
            (column * 0.4).clamp(drawer_height(5.0), room - work(6.0)),
            room - work(6.0),
        )
    } else {
        (room - work(4.0), room - floor)
    };
    let lines = |height: f32| (height - drawer_height(0.0)) / line_height();
    // A hundredth of a line either way is what a layout's sums round, not a line.
    let up = (lines(would) - 0.01).ceil().max(1.0);
    let fits = (lines(most.min(room - floor)) + 0.01).floor();
    let drawer = if fits >= 1.0 {
        drawer_height(up.min(fits))
    } else {
        row_height()
    };
    (room - drawer, drawer)
}

/// What the drawer's line takes out of the grid's share: the line and the room over it. The
/// page takes it out of the room it judges the cell bar on, so in a window too short for both
/// the bar gives way first: the drawer is the one way to a table's records and As of.
pub(super) fn least() -> f32 {
    GAP + row_height()
}

/// What the list last followed: the table, the scope and the selection it was laid out for.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Follow {
    sheet: &'static str,
    scope: Scope,
    row: Option<String>,
    column: Option<String>,
}

impl Follow {
    /// A Row scope's list is its row's whichever of its cells is selected.
    fn of(sheet: &'static str, scope: Scope, selection: Selection<'_>) -> Self {
        Self {
            sheet,
            scope,
            row: selection.map(|(row, _)| row.to_owned()),
            column: selection
                .filter(|_| scope != Scope::Row)
                .and_then(|(_, column)| column)
                .map(str::to_owned),
        }
    }
}

/// A line pressed whose row the grid does not show: what the head says, while the table, the
/// selection and the find are the ones it was pressed under.
struct Missing {
    words: String,
    sheet: &'static str,
    row: Option<String>,
    column: Option<String>,
    find: String,
}

/// The key a line answers to, where it has one of its own: its cell's press, its record's
/// head, or its staged cell's press. A day head, a note and the head over the staged cells have
/// none: their words repeat, or they stand for no one record.
fn key_of(line: &Line) -> Option<String> {
    match line {
        Line::One {
            operation, cell, ..
        }
        | Line::Cell {
            operation, cell, ..
        } => Some(format!(
            "{LINE}{}/{}",
            aterm_codec::hex::encode_lower(operation),
            cell.index
        )),
        Line::Head { operation, .. } => Some(format!(
            "{FOLD}{}",
            aterm_codec::hex::encode_lower(operation)
        )),
        Line::NotWritten { pk, column, .. } => Some(format!("{STAGED}{pk}/{column}")),
        Line::NotWrittenHead | Line::Day(_) | Line::Unselected | Line::Start => None,
    }
}

/// The keys of every control `line` lays out, which the keys can rest on: its press (a record's
/// head's only where the record folds), a record's As of, a staged cell's Undo.
fn controls(line: &Line) -> Vec<String> {
    match line {
        Line::One { operation, .. } => {
            let hex = aterm_codec::hex::encode_lower(operation);
            key_of(line)
                .into_iter()
                .chain([format!("{AS_OF}{hex}")])
                .collect()
        }
        Line::Head {
            operation, open, ..
        } => {
            let hex = aterm_codec::hex::encode_lower(operation);
            open.map(|_| format!("{FOLD}{hex}"))
                .into_iter()
                .chain([format!("{AS_OF}{hex}")])
                .collect()
        }
        Line::NotWritten { pk, column, .. } => {
            vec![
                format!("{STAGED}{pk}/{column}"),
                format!("{UNDO}{pk}/{column}"),
            ]
        }
        Line::Cell { .. } => key_of(line).into_iter().collect(),
        Line::NotWrittenHead | Line::Day(_) | Line::Unselected | Line::Start => Vec::new(),
    }
}

/// A line of the last layout, by its key ([`key_of`]) and where it stood, counted from the top.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Placed {
    key: String,
    at: usize,
}

/// The drawer's state: the view's alone, kept nowhere and written nowhere.
#[derive(Default)]
pub(crate) struct Drawer {
    pub(super) open: bool,
    /// Whether the last layout drew the drawer, as the page judged it from the window and the
    /// filters' chips while any stands, never from a selection or a change staged: the pane
    /// offers its History of this row only then.
    pub(super) drawn: Cell<bool>,
    pub(super) scope: Scope,
    /// How far its list is scrolled; a layout moves it to bring the selected cell's line into
    /// view ([`Self::follow`]).
    scroll: Cell<f32>,
    /// How far its last layout could scroll, and the room its list had.
    limit: Cell<f32>,
    room: Cell<f32>,
    /// The records a person opened or closed, by their digest: a record not named is folded
    /// where it folds.
    folds: BTreeMap<[u8; 32], bool>,
    followed: RefCell<Option<Follow>>,
    /// The read the last layout was of ([`TablesApp::reads`]), the first line it showed, and
    /// the line that wore the selected cell's ring with whether it showed: a read that puts
    /// lines above them moves the list by as many ([`Self::follow`]).
    read: Cell<u64>,
    top: RefCell<Option<Placed>>,
    ring: RefCell<Option<(Placed, bool)>>,
    /// How wide the writer's and the place's words of the list painted so far are, by the list
    /// they were measured for: the columns only widen while that list scrolls, so its values
    /// stand still under a wheel.
    fitted: RefCell<Option<(Follow, (f32, f32))>>,
    /// Where each control of the last layout's list stands: its line, counted from the top.
    places: RefCell<BTreeMap<String, usize>>,
    /// The first control of each line that has one, top down: Up and Down move among them.
    stops: RefCell<Vec<String>>,
    missing: Option<Missing>,
}

impl Drawer {
    /// Show `scope`, its list from the top.
    fn show(&mut self, scope: Scope) {
        self.open = true;
        if self.scope != scope {
            self.scope = scope;
            self.scroll.set(0.0);
        }
        self.missing = None;
    }

    /// Another table is shown: its list starts at its top, and a note on the last one goes.
    pub(super) fn another_table(&mut self) {
        self.scroll.set(0.0);
        self.missing = None;
    }

    /// A wheel over the drawer: its list `lines` further down.
    pub(super) fn wheel(&mut self, lines: i32) {
        let to = self.scroll.get() + lines as f32 * line_height();
        self.scroll.set(to.clamp(0.0, self.limit.get()));
    }

    /// Scroll so line `at` of the list shows whole.
    fn show_line(&self, at: usize) {
        let line = line_height();
        let top = at as f32 * line;
        let room = self.room.get();
        let scroll = self.scroll.get();
        if top < scroll {
            self.scroll.set(top);
        } else if room > 0.0 && top + line > scroll + room {
            self.scroll.set(top + line - room);
        }
    }

    /// Bring the control `key` of the list into view, as focus reaching it does; whether the
    /// list moved.
    pub(super) fn reveal(&self, key: &str) -> bool {
        let Some(at) = self.places.borrow().get(key).copied() else {
            return false;
        };
        let scroll = self.scroll.get();
        self.show_line(at);
        scroll != self.scroll.get()
    }

    /// How wide the writer's and the place's columns of `lines` are ([`fit`]): as wide as the
    /// words of the lines painted at `scroll`, and of those the same list painted before.
    fn fit(
        &self,
        sheet: &'static str,
        selection: Selection<'_>,
        lines: &[Line],
        scroll: f32,
        now: u64,
    ) -> (f32, f32) {
        // A Table scope's list is the table's, whichever cell is selected.
        let list = Follow::of(
            sheet,
            self.scope,
            selection.filter(|_| self.scope != Scope::Table),
        );
        let room = self.room.get();
        let (by, place) = fit(lines, |n| painted(n, scroll, room), now);
        let mut fitted = self.fitted.borrow_mut();
        let (was_by, was_place) = match fitted.as_ref() {
            Some((of, was)) if *of == list => *was,
            _ => (0.0, 0.0),
        };
        let now = (by.max(was_by), place.max(was_place));
        *fitted = Some((list, now));
        now
    }

    /// Lay the list out for the table, scope and selection in force, of read `read`. A list of
    /// another table or scope starts at its top, and so does a Row or Cell scope's once its row
    /// or cell is another; at Table scope the line that wears the selected cell's ring comes
    /// into view. A selection the list was already laid out for moves nothing, so a wheel is
    /// not undone, except as a read moves it: lines a read put above the ring's line, where it
    /// showed, or else above the first line shown, move the list down by as many, so what
    /// showed stays where it stood (at the list's top they show instead), and a ring that a
    /// read moved to another line, a record that set the selected cell, brings that line into
    /// view.
    fn follow(&self, sheet: &'static str, selection: Selection<'_>, lines: &[Line], read: u64) {
        let now = Follow::of(sheet, self.scope, selection);
        let ring = lines.iter().position(Line::ringed).and_then(|at| {
            Some(Placed {
                key: key_of(&lines[at])?,
                at,
            })
        });
        let mut followed = self.followed.borrow_mut();
        if followed.as_ref() == Some(&now) {
            if read != self.read.get() {
                let was_ring = self.ring.borrow().clone();
                let anchor = match &was_ring {
                    Some((placed, true)) => Some(placed.clone()),
                    _ if self.scroll.get() > 0.0 => self.top.borrow().clone(),
                    _ => None,
                };
                if let Some(Placed { key, at: was }) = anchor
                    && let Some(at) = lines
                        .iter()
                        .position(|line| key_of(line).as_deref() == Some(key.as_str()))
                {
                    let by = (at as f32 - was as f32) * line_height();
                    self.scroll.set((self.scroll.get() + by).max(0.0));
                }
                if let Some(placed) = &ring
                    && was_ring.as_ref().map(|(was, _)| &was.key) != Some(&placed.key)
                {
                    self.show_line(placed.at);
                }
            }
        } else {
            let same_list = followed
                .as_ref()
                .is_some_and(|was| was.sheet == now.sheet && was.scope == now.scope);
            if !same_list || self.scope != Scope::Table {
                self.scroll.set(0.0);
            }
            if self.scope == Scope::Table
                && let Some(placed) = &ring
            {
                self.show_line(placed.at);
            }
            *followed = Some(now);
        }
        self.read.set(read);
        // Where the ring stands is settled with the scroll ([`Self::settle`]).
        *self.ring.borrow_mut() = ring.map(|placed| (placed, false));
    }

    /// Note what the list laid out at `scroll` shows: its first line with a key of its own, and
    /// whether the ring's line is in view, for the next read to move the list by
    /// ([`Self::follow`]).
    fn settle(&self, lines: &[Line], scroll: f32) {
        let line = line_height();
        let first = (scroll / line).floor().max(0.0) as usize;
        *self.top.borrow_mut() = lines.iter().enumerate().skip(first).find_map(|(at, each)| {
            Some(Placed {
                key: key_of(each)?,
                at,
            })
        });
        let room = self.room.get();
        if let Some((placed, shown)) = self.ring.borrow_mut().as_mut() {
            let top = placed.at as f32 * line;
            *shown = top + line > scroll && top < scroll + room;
        }
    }

    /// The line `by` lines from the one whose control `focus` is, among those with a control:
    /// its first control. `None` where `focus` is none of the list's.
    pub(super) fn step(&self, focus: &str, by: i32) -> Option<String> {
        let places = self.places.borrow();
        let at = *places.get(focus)?;
        let stops = self.stops.borrow();
        // Each line's first control is its stop, so the focused control's line has one.
        let here = stops.iter().position(|key| places.get(key) == Some(&at))?;
        let to = (here as i64 + i64::from(by)).clamp(0, stops.len() as i64 - 1);
        stops.get(to as usize).cloned()
    }
}

impl TablesViewState {
    /// The grid's selection, as the drawer reads it.
    fn history_selection(&self) -> Selection<'_> {
        self.row.as_deref().map(|row| (row, self.column.as_deref()))
    }

    /// Where the keys go after a press on the drawer's `key`, a control still drawn after it:
    /// [`Self::rest_focus`]'s rule, except that after a pointer's press while a cell is open
    /// they return to the open cell, which still holds what is being typed.
    fn history_rest(&mut self, key: &str, keyboard: bool) {
        if !keyboard && self.editing.is_some() {
            self.focus(super::EDIT);
        } else {
            self.rest_focus(key, keyboard);
        }
    }

    /// Where the keys rest on a control of the open drawer's list whose line the list no longer
    /// lays out, they rest on Hide history, a control still drawn, as after an Undo that goes
    /// with its line: left there, Return would reach the page's default button and Up and Down
    /// would find no line to move from. A line goes under the keys for no press of theirs: the
    /// Columns move the selection off a column a person hid ([`Self::settle_columns`]), and the
    /// Cell scope lists another cell's lines; a Write's answer makes a staged line a written
    /// one; a read drops a staged cell it can no longer write. Whether the keys moved.
    pub(crate) fn settle_history_keys(&mut self, app: &TablesApp) -> bool {
        if !self.history.open {
            return false;
        }
        let Some(focus) = self.common.last_focus.as_ref().map(|key| key.as_str()) else {
            return false;
        };
        if ![LINE, STAGED, UNDO, FOLD, AS_OF]
            .iter()
            .any(|control| focus.starts_with(control))
        {
            return false;
        }
        let Some(sheet) = app.current(self).and_then(|name| app.sheet(name)) else {
            return false;
        };
        // The list's lines as the next layout lays them out ([`drawer`]); a staged cell's words
        // do not touch its keys.
        let staged: BTreeMap<At, String> = app
            .staged
            .iter()
            .filter(|((table, ..), _)| table == sheet.name)
            .map(|(at, text)| (at.clone(), text.clone()))
            .collect();
        let (_, lines) = lines(
            sheet,
            self.history.scope,
            self.history_selection(),
            &staged,
            &self.history.folds,
            app.as_of().map(|at| at.operation),
        );
        if lines
            .iter()
            .any(|line| controls(line).iter().any(|key| key == focus))
        {
            return false;
        }
        self.focus(HIDE);
        true
    }
}

impl TablesApp {
    /// A press on the drawer, or on the pane's button that opens it; `None` for a key that is
    /// neither (a record's As of is the page's own).
    pub(super) fn history_act(&mut self, view: &mut TablesViewState, key: &str) -> Option<bool> {
        if key == OF_ROW {
            view.history.show(Scope::Row);
            return Some(true);
        }
        // Pressed from the keyboard, the keys stay in the drawer; after a pointer's press they
        // are the selected cell's again, as after a press on a header.
        let keyboard = view.keyboard_on(key);
        if key == SHOW {
            let scope = view.history.scope;
            view.history.show(scope);
            view.history_rest(HIDE, keyboard);
        } else if key == HIDE {
            view.history.open = false;
            view.history.missing = None;
            view.history_rest(SHOW, keyboard);
        } else if let Some(slug) = key.strip_prefix(SCOPE) {
            let scope = Scope::ALL.into_iter().find(|scope| scope.slug() == slug)?;
            view.history.show(scope);
            view.history_rest(key, keyboard);
        } else if let Some(hex) = key.strip_prefix(FOLD) {
            let operation = self.history_operation(view, hex)?;
            let open = view.history.folds.get(&operation).copied().unwrap_or(false);
            view.history.folds.insert(operation, !open);
            view.history_rest(key, keyboard);
        } else if let Some((hex, index)) =
            key.strip_prefix(LINE).and_then(|rest| rest.split_once('/'))
        {
            let operation = self.history_operation(view, hex)?;
            let sheet = self.sheet(view.sheet)?;
            let cell = sheet
                .written
                .iter()
                .find(|written| written.operation == operation)?
                .cells
                .get(index.parse::<usize>().ok()?)?;
            let (pk, column) = (cell.pk.clone(), cell.column.clone());
            self.history_select(view, key, &pk, &column);
        } else if let Some((pk, column)) = key
            .strip_prefix(STAGED)
            .and_then(|rest| rest.split_once('/'))
        {
            self.history_select(view, key, pk, column);
        } else if let Some((pk, column)) =
            key.strip_prefix(UNDO).and_then(|rest| rest.split_once('/'))
        {
            let pressed = view
                .common
                .last_focus
                .as_ref()
                .is_some_and(|focus| focus.as_str() == key);
            if born(pk, column) {
                // A new row's birth goes as its pane's Remove takes it: the row and every cell
                // staged in it. Not while a Write is out, as Remove is not.
                if self.writing {
                    return Some(true);
                }
                let pk = pk.to_owned();
                self.drop_row(view.sheet, &pk);
                if view.row.as_deref() == Some(pk.as_str()) {
                    view.row = None;
                    view.column = None;
                }
            } else {
                self.unstage(&(view.sheet.to_owned(), pk.to_owned(), column.to_owned()));
            }
            // Its Undo goes with its line: keys left on it would reach the page's default
            // button. Whatever pressed it, they rest on a control still drawn: from the
            // keyboard Hide history, after a pointer the selected cell (Hide history where none
            // is), as the pane's Undo returns them to the cell.
            if pressed {
                view.history_rest(HIDE, keyboard);
            }
        } else {
            return None;
        }
        Some(true)
    }

    /// The record of the shown table whose digest reads `hex`.
    fn history_operation(&self, view: &TablesViewState, hex: &str) -> Option<[u8; 32]> {
        self.sheet(view.sheet)?
            .written
            .iter()
            .map(|written| written.operation)
            .find(|operation| aterm_codec::hex::encode_lower(operation) == hex)
    }

    /// Select the grid's cell at `pk`, `column`, as a press on it does: it is the selection, it
    /// comes into view and the pane follows; no cell opens. Filters on the columns' values that
    /// alone leave the row out are cleared first, by the rule Check's Show row follows
    /// ([`TablesViewState::clear_filters_for`]). Where the grid does not show the row otherwise
    /// (the find or the lifecycle filter leaves it out, or it is no longer live), the filters and
    /// the selection stay and the head names the row. A line names a cell, and seeing it
    /// is what the press asks: its column is shown as its line in the Columns list shows it (a
    /// column the list hid is hidden no longer, one no row fills is pinned, so the fold leaves
    /// it drawn after the selection leaves it: [`TablesViewState::show_column`]), and the grid
    /// scrolls across to it once the columns settle ([`TablesViewState::settle_columns`]). A
    /// column the grid has none of (a birth's lifecycle, where the table shows none) gives the
    /// row's key cell.
    fn history_select(&mut self, view: &mut TablesViewState, key: &str, pk: &str, column: &str) {
        let Some(sheet) = self.sheet(view.sheet) else {
            return;
        };
        let column = if sheet.columns.iter().any(|c| c.name == column) {
            column.to_owned()
        } else {
            sheet.key.clone()
        };
        // Pressed from the keyboard, the keys stay on the line; a pointer's press leaves them
        // on the cell, as a press on a header does.
        let keyboard = view.keyboard_on(key);
        // The row asked for shows as Check's Show row shows one: the filters on the columns'
        // values give way where they alone leave it out. The find and the lifecycle filter stay,
        // and a row they leave out is named in the head, the filters kept: clearing them would
        // show no row.
        view.clear_filters_for(self, sheet, pk);
        view.put_in_order(self, sheet);
        let at = view.order.borrow().keys.iter().position(|row| row == pk);
        let Some(at) = at else {
            let name = row_words(sheet, &names(sheet), pk);
            view.history.missing = Some(Missing {
                words: format!("{name} is not in the rows shown"),
                sheet: view.sheet,
                row: view.row.clone(),
                column: view.column.clone(),
                find: view.find.value().to_owned(),
            });
            // Nothing changed: the keys go back where the selection, or an open cell, has them.
            view.history_rest(key, keyboard);
            return;
        };
        view.close_edit(self, false);
        if let Some(sheet) = self.sheet(view.sheet) {
            view.show_column(self, sheet, &column);
        }
        view.select(pk, &column);
        view.keep_in_view(at);
        if keyboard {
            view.focus(key);
        }
        view.history.missing = None;
        // The pressed line is already in view: the list does not jump to the cell's newest.
        let followed = Follow::of(view.sheet, view.history.scope, Some((pk, Some(&column))));
        view.history.followed.replace(Some(followed));
    }
}

/// A seat `width` wide for a line's press and the words painted over it.
fn seat(key: String, width: f32, children: Vec<UiNode>) -> UiNode {
    group(key)
        .layout(Layout::default().width(Length::Fixed(width)))
        .children(children)
}

/// Words painted over a press, set `left` in from the seat's edge.
fn face(key: String, left: f32, children: Vec<UiNode>) -> UiNode {
    group(key)
        .layout(Layout::row().gap(6.0).padding(Insets {
            left,
            ..Insets::default()
        }))
        .children(children)
        .paint_only_tree()
}

/// A button `width` wide at the end of a line, clear of the lines above and below.
fn at_end(key: String, width: f32, inner: UiNode) -> UiNode {
    group(key)
        .layout(
            Layout::row()
                .width(Length::Fixed(width))
                .padding(Insets::symmetric(0.0, 2.0)),
        )
        .children(vec![inner])
}

/// The widths a line's parts take.
struct Widths {
    /// The press and its words.
    seat: f32,
    /// The button at the line's end.
    end: f32,
    /// The writer and the time, on a one-cell record's line.
    by: f32,
    /// The row's key text and the column's words.
    place: f32,
    /// The value written, on a one-cell record's line.
    value: f32,
    /// The value written, on a line set in under its head.
    under: f32,
}

impl Widths {
    /// In `inner`, with the writer's and the place's columns as wide as `fit` says their words
    /// are ([`fit`]), so each value stands right after its cell's name.
    fn of(inner: f32, (by_fit, place_fit): (f32, f32)) -> Self {
        let end = native_button_label_width("As of").max(native_button_label_width("Undo")) + 24.0;
        let seat = (inner - end - 8.0).max(1.0);
        let by = by_fit.min(seat * 0.44).min(360.0 * scale());
        let place = place_fit.min(seat * 0.3).min(280.0 * scale());
        Self {
            seat,
            end,
            by,
            place,
            value: (seat - 8.0 - by - place - 20.0).max(1.0),
            under: (seat - 8.0 - INDENT - place - 14.0).max(1.0),
        }
    }
}

/// How wide the writer's words and the place's are on the lines of `lines` that `near` holds
/// of, where their lines paint them: (writer and time, place).
fn fit(lines: &[Line], near: impl Fn(usize) -> bool, now: u64) -> (f32, f32) {
    let place = |text: &str, style: StyleRef| text_width(text, SemanticRole::Text, style) + 2.0;
    let (mut by_w, mut place_w) = (0.0_f32, 0.0_f32);
    for (_, line) in lines.iter().enumerate().filter(|(n, _)| near(*n)) {
        match line {
            Line::One { by, cell, .. } => {
                by_w = by_w.max(writer_width(by, now));
                place_w = place_w.max(place(&cell.place, StyleRef::Note));
            }
            Line::Cell { cell, .. } => place_w = place_w.max(place(&cell.place, StyleRef::Note)),
            Line::NotWritten { place: at, .. } => {
                place_w = place_w.max(place(at, StyleRef::Unwritten));
            }
            _ => {}
        }
    }
    (by_w, place_w)
}

/// How wide [`writer`]'s words for `by` are, whole: its Mac's name, the room between, and when.
fn writer_width(by: &Author, now: u64) -> f32 {
    text_width(&by.who(), SemanticRole::Text, by.style(StyleRef::Plain))
        + 8.0
        + text_width(
            &format!("\u{b7} {}", by.when(now)),
            SemanticRole::Text,
            StyleRef::Plain,
        )
        + 2.0
}

/// What a layout of the list needs beside its lines.
struct Look {
    widths: Widths,
    scroll: f32,
    room: f32,
    now: u64,
    /// Whether the page may move to another point ([`TablesApp::movable`]).
    movable: bool,
    /// The staged cells of the shown table whose value its rules refuse, by row and column,
    /// with the rule ([`TablesApp::refusal`]).
    refused: BTreeMap<(String, String), String>,
    /// Whether a Write is out: a new row's Undo waits for it.
    writing: bool,
}

/// Whether line `n` of a list scrolled `scroll` in `room` paints: it shows, or stands within
/// six lines of what does.
fn painted(n: usize, scroll: f32, room: f32) -> bool {
    let line = line_height();
    let (top, over) = (n as f32 * line, line * 6.0);
    top + line >= scroll - over && top <= scroll + room + over
}

/// How thick the bar is down the list's right edge that says how much of it shows, and where.
const THUMB: f32 = 3.0;

/// A cell's place and value, as a line paints them.
fn entry_words(key: &str, cell: &Entry, place_w: f32, value_w: f32) -> [UiNode; 2] {
    [
        fitted(format!("{key}/place"), &cell.place, StyleRef::Note, place_w),
        fitted(
            format!("{key}/value"),
            &cell.value,
            if cell.cleared {
                StyleRef::Faint
            } else {
                StyleRef::Plain
            },
            value_w,
        ),
    ]
}

/// What a cell's line is named: its writer and when, then the cell and the value written, so
/// each line says which record it is of, the line under a head as the head's own line does.
fn line_words(by: &Author, cell: &Entry, now: u64) -> String {
    format!(
        "{} \u{b7} {}, {}: {}",
        by.who(),
        by.when(now),
        cell.place,
        cell.value
    )
}

/// A record's As of: the table as its causal past holds it. Off for the record the page shows
/// as of, and while cells are staged or being written.
fn as_of_button(operation: &str, by: &Author, here: bool, look: &Look) -> UiNode {
    at_end(
        format!("{AS_OF}{operation}/seat"),
        look.widths.end,
        button(
            &format!("{AS_OF}{operation}"),
            "As of",
            &format!("The table as of {} \u{b7} {}", by.who(), by.when(look.now)),
            StyleRef::Secondary,
            !here && look.movable,
        ),
    )
}

/// The list's nodes, one of a line's height for each line, where each of its controls stands,
/// and each line's first control, top down. A line far from what the list shows keeps its place
/// and its controls, which name it to the keyboard and to assistive technology, and paints
/// nothing.
#[expect(clippy::too_many_lines, reason = "one list, laid out in one pass")]
fn nodes(lines: &[Line], look: &Look) -> (Vec<UiNode>, BTreeMap<String, usize>, Vec<String>) {
    let line = line_height();
    let w = &look.widths;
    let whole = w.seat + 8.0 + w.end;
    let mut places = BTreeMap::new();
    let mut stops = Vec::new();
    let mut out = Vec::with_capacity(lines.len());
    for (n, each) in lines.iter().enumerate() {
        for key in controls(each) {
            places.insert(key, n);
        }
        let near = painted(n, look.scroll, look.room);
        let note = |key: String, text: &str| {
            let words = if near {
                vec![fitted(key.clone(), text, StyleRef::Note, whole - 8.0)]
            } else {
                Vec::new()
            };
            group(format!("{key}/line"))
                .layout(Layout::row().height(Length::Fixed(line)).padding(Insets {
                    left: 8.0,
                    ..Insets::default()
                }))
                .children(words)
        };
        let row = |key: String, children: Vec<UiNode>| {
            group(key)
                .layout(Layout::row().gap(8.0).height(Length::Fixed(line)))
                .children(children)
        };
        let style = |here: bool| {
            if here {
                StyleRef::Primary
            } else {
                StyleRef::Plain
            }
        };
        out.push(match each {
            Line::NotWrittenHead => note("tables/history/unwritten".to_owned(), "NOT WRITTEN"),
            Line::Day(day) => note(format!("tables/history/day/{n}"), day),
            Line::Unselected => note(
                "tables/history/unselected".to_owned(),
                "Select a cell to see its history",
            ),
            Line::Start => note(
                "tables/history/start".to_owned(),
                "Start of the records read here",
            ),
            Line::NotWritten {
                pk,
                column,
                place,
                text,
            } => {
                let key = format!("{STAGED}{pk}/{column}");
                let undo = format!("{UNDO}{pk}/{column}");
                stops.push(key.clone());
                // A staged value the table's rules refuse says so in the grid's word and ink.
                let refused = look.refused.get(&(pk.clone(), column.clone()));
                let mut spoken = format!("{place}: {text}, not written");
                if let Some(rule) = refused {
                    spoken.push_str(&format!(", refused: {rule}"));
                }
                let mut over_press = vec![press(&key, spoken)];
                if near {
                    let tag_w = refused.map_or(0.0, |_| {
                        text_width("refused", SemanticRole::Text, StyleRef::Danger) + 6.0
                    });
                    let mut words = vec![
                        fitted(format!("{key}/place"), place, StyleRef::Unwritten, w.place),
                        fitted(
                            format!("{key}/text"),
                            text,
                            StyleRef::Unwritten,
                            (w.under - tag_w).max(1.0),
                        ),
                    ];
                    if refused.is_some() {
                        words.push(fitted(
                            format!("{key}/refused"),
                            "refused",
                            StyleRef::Danger,
                            tag_w,
                        ));
                    }
                    over_press.push(face(format!("{key}/face"), 8.0 + INDENT, words));
                }
                row(
                    format!("{key}/line"),
                    vec![
                        styled(format!("{key}/seat"), StyleRef::Unwritten)
                            .layout(Layout::default().width(Length::Fixed(w.seat)))
                            .children(over_press),
                        at_end(
                            format!("{undo}/seat"),
                            w.end,
                            // A new row's Undo takes the row back, as its pane's Remove does,
                            // and is off while a Write is out, as Remove is.
                            if born(pk, column) {
                                button(
                                    &undo,
                                    "Undo",
                                    &format!(
                                        "Undo: take back this {} and every cell staged in it",
                                        place.to_lowercase()
                                    ),
                                    StyleRef::Secondary,
                                    !look.writing,
                                )
                            } else {
                                button(
                                    &undo,
                                    "Undo",
                                    &format!("Undo {place}"),
                                    StyleRef::Secondary,
                                    true,
                                )
                            },
                        ),
                    ],
                )
            }
            Line::One {
                operation,
                by,
                here,
                cell,
            } => {
                let hex = aterm_codec::hex::encode_lower(operation);
                let key = format!("{LINE}{hex}/{}", cell.index);
                stops.push(key.clone());
                // The ring is painted over the press, as the grid's cell wears it: no selected
                // state, whose chip paint is the accent.
                let mut over_press = vec![press(&key, line_words(by, cell, look.now))];
                if near {
                    let mut words: Vec<UiNode> =
                        writer(&key, by, &by.when(look.now), style(*here), w.by).into();
                    words.extend(entry_words(&key, cell, w.place, w.value));
                    over_press.push(face(format!("{key}/face"), 8.0, words));
                    if cell.ringed {
                        over_press.push(
                            styled(format!("{key}/focus"), StyleRef::CellSelected).paint_only(),
                        );
                    }
                }
                row(
                    format!("{key}/line"),
                    vec![
                        seat(format!("{key}/seat"), w.seat, over_press),
                        as_of_button(&hex, by, *here, look),
                    ],
                )
            }
            Line::Head {
                operation,
                by,
                here,
                cells,
                rows,
                open,
                ringed,
            } => {
                let hex = aterm_codec::hex::encode_lower(operation);
                let key = format!("{FOLD}{hex}");
                stops.push(if open.is_some() {
                    key.clone()
                } else {
                    format!("{AS_OF}{hex}")
                });
                let mut count = format!("\u{b7} {}", cells_words(*cells));
                if *rows > 1 {
                    count.push_str(&format!(" in {rows} rows"));
                }
                // The count follows the time as one run of words: the writer's words take what
                // they need, and the count the rest, less the room a fold's chevron stands in.
                let room = (w.seat - 8.0 - if open.is_some() { 34.0 } else { 0.0 }).max(1.0);
                let by_w = writer_width(by, look.now).min(room * 0.75);
                let count_w = (room - by_w - 6.0).max(1.0);
                let words = |key: &str| {
                    let mut words: Vec<UiNode> =
                        writer(key, by, &by.when(look.now), style(*here), by_w).into();
                    words.push(fitted(
                        format!("{key}/count"),
                        &count,
                        StyleRef::Note,
                        count_w,
                    ));
                    words
                };
                let held = match open {
                    // A record that folds: its head is the button that opens and closes it.
                    Some(open) => {
                        let mut over_press = vec![UiNode::new(
                            key.clone(),
                            UiContent::Button(
                                Control::new(
                                    ButtonSpec::new(format!(
                                        "{} \u{b7} {} {count}",
                                        by.who(),
                                        by.when(look.now)
                                    ))
                                    .visual_label(String::new()),
                                    ActionId::new(key.clone()),
                                )
                                .state(ControlState {
                                    expanded: Some(*open),
                                    ..ControlState::default()
                                }),
                            ),
                        )];
                        if near {
                            over_press.push(face(format!("{key}/face"), 8.0, words(&key)));
                            if *ringed {
                                over_press.push(
                                    styled(format!("{key}/focus"), StyleRef::CellSelected)
                                        .paint_only(),
                                );
                            }
                        }
                        over_press
                    }
                    None if near => vec![
                        group(format!("{key}/words"))
                            .layout(Layout::row().gap(6.0).padding(Insets {
                                left: 8.0,
                                ..Insets::default()
                            }))
                            .children(words(&key)),
                    ],
                    None => Vec::new(),
                };
                row(
                    format!("{key}/line"),
                    vec![
                        seat(format!("{key}/seat"), w.seat, held),
                        as_of_button(&hex, by, *here, look),
                    ],
                )
            }
            Line::Cell {
                operation,
                by,
                cell,
            } => {
                let hex = aterm_codec::hex::encode_lower(operation);
                let key = format!("{LINE}{hex}/{}", cell.index);
                stops.push(key.clone());
                // Named as a one-cell record's line is: its head may be off the list's screen.
                let mut over_press = vec![press(&key, line_words(by, cell, look.now))];
                if near {
                    // Set in under its record's head, the value right after its cell's name.
                    over_press.push(face(
                        format!("{key}/face"),
                        8.0 + INDENT,
                        entry_words(&key, cell, w.place, w.under).into(),
                    ));
                    if cell.ringed {
                        over_press.push(
                            styled(format!("{key}/focus"), StyleRef::CellSelected).paint_only(),
                        );
                    }
                }
                row(
                    format!("{key}/line"),
                    vec![seat(format!("{key}/seat"), w.seat, over_press)],
                )
            }
        });
    }
    (out, places, stops)
}

/// The three-way scope control: one segment for each scope, the current one selected.
fn scope_control(current: Scope, width: f32) -> UiNode {
    let each = ((width - 4.0) / 3.0).max(1.0);
    let segments = Scope::ALL
        .into_iter()
        .map(|scope| {
            let key = format!("{SCOPE}{}", scope.slug());
            let on = scope == current;
            let mut seat = Vec::with_capacity(3);
            if on {
                seat.push(styled(format!("{key}/band"), StyleRef::Code));
            }
            // A press under painted words, as the rail's line is: a selected state would paint
            // the accent's chip, so the choice is the segment's value, which assistive
            // technology says and the segmented control's one Tab stop goes to.
            seat.push(UiNode::new(
                key.clone(),
                UiContent::Button(
                    Control::new(
                        ButtonSpec::new(scope.spoken()).visual_label(String::new()),
                        ActionId::new(key.clone()),
                    )
                    .value(SemanticValue::Bool(on))
                    .style(StyleRef::Navigation),
                ),
            ));
            let style = if on {
                StyleRef::Primary
            } else {
                StyleRef::Plain
            };
            let word_w =
                (text_width(scope.word(), SemanticRole::Text, style) + 2.0).min(each - 8.0);
            seat.push(
                group(format!("{key}/face"))
                    .layout(Layout::row())
                    .children(vec![
                        group(format!("{key}/lead")).layout(Layout::default().width(Length::Fill)),
                        fitted(format!("{key}/word"), scope.word(), style, word_w.max(0.0)),
                        group(format!("{key}/trail")).layout(Layout::default().width(Length::Fill)),
                    ])
                    .paint_only_tree(),
            );
            group(format!("{key}/seat"))
                .layout(Layout::default().width(Length::Fixed(each)))
                .children(seat)
        })
        .collect();
    UiNode::new(
        "tables/history/scope",
        UiContent::Group(GroupSpec {
            label: Some("History of".to_owned()),
            role: SemanticRole::Group,
            style: StyleRef::Secondary,
        }),
    )
    .layout(Layout::row().gap(2.0).width(Length::Fixed(width)))
    .children(segments)
    .segmented()
}

/// The drawer's card, `height` tall: its head's line, and its list where it is open.
fn card(head: Vec<UiNode>, list: Option<UiNode>, height: f32) -> UiNode {
    let mut children = vec![
        group("tables/history/head")
            .layout(
                Layout::row()
                    .gap(12.0)
                    .height(Length::Fixed(row_height()))
                    .padding(Insets::symmetric(0.0, 2.0)),
            )
            .children(head),
    ];
    children.extend(list);
    styled(KEY, StyleRef::Secondary)
        .layout(
            Layout::column()
                .height(Length::Fixed(height))
                .padding(Insets::symmetric(10.0, 0.0))
                .clipped(),
        )
        .children(children)
}

/// The drawer under the grid, `width` wide and `height` tall ([`heights`]).
pub(super) fn drawer(
    app: &TablesApp,
    view: &TablesViewState,
    sheet: &Sheet,
    width: f32,
    height: f32,
) -> UiNode {
    let now = aterm_time::unix_millis().unwrap_or(0);
    let state = &view.history;
    let inner = (width - 20.0).max(1.0);
    // In a head too narrow for all of it the words give way, and the button that shows or
    // hides the list stays whole: the newest record's words or the counts first, then the title.
    let title_full = text_width("History", SemanticRole::Text, StyleRef::Plain);
    let title = |room: f32| {
        let title_w = title_full.min(room.max(0.0));
        (
            fitted("tables/history/title", "History", StyleRef::Plain, title_w),
            title_w,
        )
    };
    if !state.open {
        let show_w = native_button_label_width("Show history") + 24.0;
        let room = (inner - show_w - 24.0).max(0.0);
        let (title, title_w) = title(room);
        return card(
            vec![
                title,
                fitted(
                    "tables/history/newest",
                    &newest_words(sheet, now),
                    StyleRef::Note,
                    (room - title_w).max(0.0),
                ),
                button(
                    SHOW,
                    "Show history",
                    "Show history",
                    StyleRef::Secondary,
                    true,
                ),
            ],
            None,
            height,
        );
    }
    let selection = view.history_selection();
    // A reference staged reads as the row it names, as its grid cell does ([`rows::reads`]).
    let staged: BTreeMap<At, String> = app
        .staged
        .iter()
        .filter(|((table, ..), _)| table == sheet.name)
        .map(|(at, text)| {
            let reads = sheet
                .columns
                .iter()
                .find(|column| column.name == at.2)
                .map_or_else(|| text.clone(), |column| rows::reads(app, column, text));
            (at.clone(), reads)
        })
        .collect();
    let (tally, lines) = lines(
        sheet,
        state.scope,
        selection,
        &staged,
        &state.folds,
        app.as_of().map(|at| at.operation),
    );
    let line = line_height();
    let room = (height - row_height() - FOOT).max(0.0);
    let whole = lines.len() as f32 * line;
    let limit = (whole - room).max(0.0);
    state.room.set(room);
    state.limit.set(limit);
    state.follow(sheet.name, selection, &lines, app.reads);
    let scroll = state.scroll.get().min(limit);
    state.scroll.set(scroll);
    state.settle(&lines, scroll);
    // The list keeps room down its right edge for the bar that says where it stands.
    let lines_w = (inner - THUMB - 4.0).max(1.0);
    let (listed, places, stops) = nodes(
        &lines,
        &Look {
            widths: Widths::of(
                lines_w,
                state.fit(sheet.name, selection, &lines, scroll, now),
            ),
            scroll,
            room,
            now,
            movable: app.movable(),
            writing: app.writing,
            // A new row's birth line says the refusal its lifecycle cell carries (a key the
            // table holds already), as that cell does, though no cell stages it.
            refused: app
                .staged
                .keys()
                .filter(|(table, ..)| table == sheet.name)
                .cloned()
                .chain(
                    sheet
                        .rows
                        .iter()
                        .filter(|row| rows::is_new(&row.pk))
                        .map(|row| {
                            (
                                sheet.name.to_owned(),
                                row.pk.clone(),
                                aterm_db::typed::LIFECYCLE.to_owned(),
                            )
                        }),
                )
                .filter_map(|at| {
                    let rule = app.refusal(&at)?.clone();
                    Some(((at.1, at.2), rule))
                })
                .collect(),
        },
    );
    *state.places.borrow_mut() = places;
    *state.stops.borrow_mut() = stops;
    // The head: a line pressed whose row the grid does not show is named where the counts
    // stand, while the selection and the find are the ones it was pressed under.
    let missing = state.missing.as_ref().filter(|missing| {
        missing.sheet == view.sheet
            && missing.row == view.row
            && missing.column == view.column
            && missing.find == view.find.value()
    });
    let hide_w = native_button_label_width("Hide history") + 24.0;
    let head_room = (inner - hide_w - 36.0).max(0.0);
    // The scope's words give way last, its segments narrowing alike.
    let scope_w = (Scope::ALL
        .into_iter()
        .map(|scope| native_button_label_width(scope.word()) + 24.0)
        .fold(0.0_f32, f32::max)
        * 3.0
        + 4.0)
        .min(head_room);
    let (title, title_w) = title(head_room - scope_w);
    let count_w = (head_room - scope_w - title_w).max(0.0);
    // In a head too narrow for all its words, the counts stay and the note on them goes.
    let count = if !selected(state.scope, selection) {
        String::new()
    } else {
        let whole = tally.words(state.scope);
        if text_width(&whole, SemanticRole::Text, StyleRef::Note) <= count_w {
            whole
        } else {
            tally.count(state.scope)
        }
    };
    let head = vec![
        title,
        scope_control(state.scope, scope_w),
        fitted(
            "tables/history/count",
            missing.map_or(count.as_str(), |missing| missing.words.as_str()),
            StyleRef::Note,
            count_w,
        ),
        button(
            HIDE,
            "Hide history",
            "Hide history",
            StyleRef::Secondary,
            true,
        ),
    ];
    // One Tab stop for the whole list, however many lines it holds: Up and Down move among its
    // lines ([`Drawer::step`]), Left and Right on to a line's As of or Undo.
    let list = group("tables/history/list")
        .layout(
            Layout::column()
                .width(Length::Fixed(lines_w))
                .height(Length::Fixed(room))
                .clipped()
                .scrolled(scroll),
        )
        .room(room, limit > 0.0)
        .lists_every_row()
        .children(listed)
        .segmented();
    // A list longer than its room wears the grid's bar down its right edge: how much of it
    // shows, and where.
    let mut body = vec![list];
    if limit > 0.0 && room > 0.0 {
        let thumb = (room * room / whole).max(24.0).min(room);
        let lead = (room - thumb) * (scroll / limit);
        body.push(
            group("tables/history/track")
                .layout(Layout::column().width(Length::Fixed(THUMB)))
                .children(vec![
                    group("tables/history/track/lead")
                        .layout(Layout::default().height(Length::Fixed(lead))),
                    styled("tables/history/thumb", StyleRef::SheetBar)
                        .layout(Layout::default().height(Length::Fixed(thumb))),
                ])
                .paint_only_tree(),
        );
    }
    let body = group("tables/history/body")
        .layout(Layout::row().gap(4.0).height(Length::Fixed(room)))
        .children(body);
    card(head, Some(body), height)
}

#[cfg(all(test, unix))]
#[path = "native_tables_history_tests.rs"]
mod tests;
