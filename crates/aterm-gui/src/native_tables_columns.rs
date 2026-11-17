// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Tables page's columns (docs/PLAN.md LT-40): which of a table's columns the grid draws, and
//! how many of its rows fill each.
//!
//! - **Fill.** A column's fill is how many of the rows written that the grid shows by the
//!   lifecycle filter hold a value in it, as the grid shows that value, of how many such rows
//!   there are (`31 of 156`), so the count reads against what is on screen: the live rows, or the
//!   retired ones once the filter shows those, a row with a change staged that the filter leaves
//!   out among them, as the grid shows it. A set with no members, a linked count of none, a
//!   reference not set and a cell no row wrote are empty; a reference that is set counts (it
//!   shows the words of the row it names, or its key where the read lists no such row); a
//!   secret's cell counts the entry name it shows, and no value is read for it. What counts is
//!   what the read holds: a new row, not written, counts among no rows and in no fill, nor does
//!   any typed cell. The rows the find field leaves out count, so a fill does not move while the
//!   rows found narrow. It is counted once for each read, filter and set of rows with a change
//!   staged.
//! - **The list.** The tools row's Columns button (`Columns 15 of 21`: the columns the grid draws
//!   of all the table's) opens a list under it, laid over the page and right-aligned to it: the
//!   switch that folds the columns no row fills, with how many it folds; then each column under its
//!   group's word, a mark where the grid draws it, its name (in the note's ink where no row fills
//!   it) and its fill (`31 of 156`); then Show all, and a line that says the list writes nothing.
//!   The key column is always drawn and not offered. A press on a column hides it where the grid
//!   draws it and shows it where it does not; a column with a change not written, or a refused cell
//!   (a new row's key column left empty), is never hidden, and its line says so. Hiding the column
//!   the rows are sorted by puts them back in the table's order, since its header, which says so,
//!   goes with it. Show all shows every column and turns the fold off. The arrows move among the
//!   list's lines, Space or Return presses one, and Escape, the Columns button again or a press
//!   anywhere else closes it. Every line is a button that says what it holds. The list is a surface
//!   over the page ([`UiNode::surface`]): a press on its words or its frame, and the wheel over it,
//!   are its own, and reach nothing under it, the history drawer's lines included; Escape closes
//!   the list first, and a drawer open under it stays open. However it closes, the keys are left on
//!   a control the page still draws, never on a line it no longer does, which Return would pass to
//!   the page's default button, Write.
//! - **The fold.** On by default: the columns no row the filter shows fills fold into one narrow
//!   stub at the end of their group, headed `5 EMPTY`; a press on it unfolds that group, and only
//!   it, until the tab closes or the switch folds every group again, and the keys go to the first
//!   column it showed (after a pointer's press, to the selected cell, as a header's press leaves
//!   them). Where the filter shows no written row, nothing folds: the columns say what a row
//!   would hold.
//!   A column a change not written (a new row's typed cell among them), a refusal, the open
//!   cell, the selected cell, a range's other corner or the sort stands in is never folded, so a
//!   cell chosen from elsewhere (Check's Show row) is drawn, and scrolled into view once the grid
//!   has a place for it; nor, while a new row of the table stands, is any column it is typed
//!   into, which no written row need fill.
//! - **The grid's columns.** Copy, a range, Tab's path, the arrows, a paste and the cell bar's
//!   address go through the columns the grid draws alone ([`TablesViewState::shown_columns`]);
//!   a selected cell whose column is hidden moves to the nearest drawn column of its row, after
//!   a press and after a reply of the reader's alike (a Write that ends the change a hidden
//!   column was drawn for). A press on a line of the history drawer names a cell, and shows its
//!   column as the column's line in the list does: hidden no longer, and out of the fold where
//!   no row fills it ([`TablesViewState::show_column`]).
//! - **The header's fill.** Each column's header wears a thin bar along its lower edge in the
//!   sheet's bar ink, as long across the header as the share of rows that fill the column, and
//!   its press's name says the fill after what the press does (`Sort by Chip, 94 of 156 rows
//!   hold a value`).
//!
//! The choice is the view's, per table and kept while the tab lives: it writes nothing.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};

use super::rows::Filter;
use super::{
    At, Group, HEAD, Sheet, SheetColumn, SheetRow, TablesApp, TablesViewState, across_for,
    cell_key, field_words, fitted, group, head_cell, ordered, press, row_height, scale, styled,
    widths,
};
use crate::native_app::AppEvent;
use crate::native_ui::{
    ActionId, ButtonSpec, Control, ControlState, GroupSpec, Insets, Layout, Length, SemanticRole,
    StyleRef, UiContent, UiNode,
};
use crate::native_workers::text_width;

/// The tools row's Columns button: it opens the list and closes it.
pub(crate) const COLUMNS: &str = "tables/columns";
/// Every key of the open list starts so.
const LIST: &str = "tables/columns/";
/// The list's switch that folds the columns no row fills.
const FOLD: &str = "tables/columns/fold";
/// A column's line in the list.
pub(crate) const COLUMN: &str = "tables/columns/column/";
/// The list's Show all.
const ALL: &str = "tables/columns/all";
/// The open list's surface: a press on it that no line takes.
const PANEL: &str = "tables/columns/panel";
/// A group's folded stub in the grid's header line: a press unfolds the group.
pub(crate) const STUB: &str = "tables/fold/";
/// The name a stub stands under among the grid's columns: no column of a sheet is named so.
const STUB_COLUMN: &str = "~fold/";
/// How thick a header's fill bar is.
const FILL_BAR: f32 = 2.0;
/// How wide the open list is, at the text scale of 1.
const LIST_WIDTH: f32 = 340.0;
/// How far apart a line of the list sets its words.
pub(crate) const LINE_GAP: f32 = 6.0;

/// One table's choice of its columns: view state, kept while the tab lives.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Chosen {
    /// The columns a person hid.
    hidden: BTreeSet<String>,
    /// Empty columns a person showed one by one, which the fold leaves drawn.
    pinned: BTreeSet<String>,
    /// The groups a press on their stub unfolded.
    unfolded: BTreeSet<Group>,
    /// The list's switch turned the fold off.
    spread: bool,
}

/// Each column's fill and the rows it was counted over, and what it was counted for: the
/// sheet, its read, the lifecycle filter and the rows with a change staged, which the grid shows
/// whatever the filter.
struct Fills {
    of: (&'static str, u64, Filter, Vec<String>),
    rows: usize,
    by: BTreeMap<String, usize>,
}

/// The Columns control's view state ([`TablesViewState::columns`]).
#[derive(Default)]
pub(crate) struct ColumnsView {
    /// Each table's choice, by its name.
    tables: BTreeMap<String, Chosen>,
    /// Whether the list is open.
    pub(crate) open: bool,
    /// How far the list's lines are scrolled, how far they can be, and how tall their room is.
    scroll: f32,
    limit: Cell<f32>,
    room: Cell<f32>,
    /// Where each of the list's buttons stood in it as it was last laid out: its key, its top
    /// and its bottom. The arrows scroll the one they reach into view.
    lines: RefCell<Vec<(String, f32, f32)>>,
    /// The list's line the keys were last put on: a press on the list's surface leaves them
    /// there.
    line: Option<String>,
    /// The table the grid last laid its columns out for ([`TablesViewState::spans`]).
    laid: Cell<Option<&'static str>>,
    fills: RefCell<Option<Fills>>,
}

/// Why the grid leaves a column out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Out {
    Hidden,
    Folded,
}

/// A group's key word: what its stub's key ends in.
const fn slug(group: Group) -> &'static str {
    match group {
        Group::Stored => "stored",
        Group::Born => "born",
        Group::SetByAterm => "aterm",
        Group::Reference => "reference",
        Group::Linked => "linked",
    }
}

/// A group's word as a sentence says it: `Set by aterm`.
fn group_words(group: Group) -> String {
    let lower = group.word().to_lowercase();
    let mut chars = lower.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
        .unwrap_or_default()
}

/// `94 of 156 rows hold a value`.
pub(crate) fn fill_words(fill: usize, rows: usize) -> String {
    match (fill, rows) {
        (_, 1) => format!("{fill} of 1 row holds a value"),
        (1, _) => format!("1 of {rows} rows holds a value"),
        _ => format!("{fill} of {rows} rows hold a value"),
    }
}

/// Whether `column` is a group's folded stub, which the grid draws among its columns.
pub(crate) fn is_stub(column: &SheetColumn) -> bool {
    column.name.starts_with(STUB_COLUMN)
}

impl ColumnsView {
    fn chosen_mut(&mut self, sheet: &str) -> &mut Chosen {
        self.tables.entry(sheet.to_owned()).or_default()
    }

    /// Note that the grid laid its columns out for table `sheet`.
    pub(crate) fn laid_out(&self, sheet: &'static str) {
        self.laid.set(Some(sheet));
    }
}

/// What a cell of a column holds that keeps the column in the grid whatever the choice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Holds {
    /// A change not written: a staged cell, a new row's typed cell among them.
    Change,
    /// A refusal where nothing is staged: a new row's key is made from a column left empty, and
    /// the Write that row waits on says so at that cell, which the person must reach to fill.
    Refusal,
}

impl Holds {
    /// The words a column's line in the list says it with, and their ink.
    fn note(self) -> (&'static str, StyleRef) {
        match self {
            Self::Change => ("has a change not written", StyleRef::Unwritten),
            Self::Refusal => ("has a refused cell", StyleRef::Danger),
        }
    }
}

/// What a cell of `column` of `sheet` holds that the grid must draw: a change not written, else a
/// refusal (of the staged cells' check, or of the last Write).
fn holds(app: &TablesApp, sheet: &Sheet, column: &str) -> Option<Holds> {
    let at = |(of, _, at): &At| of == sheet.name && at == column;
    if app.staged.keys().any(at) {
        Some(Holds::Change)
    } else if app.checked.keys().chain(app.refused.keys()).any(at) {
        Some(Holds::Refusal)
    } else {
        None
    }
}

impl TablesViewState {
    /// Each column's fill of `sheet`, counted again where what it was counted for changed.
    fn fills(&self, app: &TablesApp, sheet: &Sheet) -> std::cell::Ref<'_, Fills> {
        let filter = self.filter(sheet);
        // The rows with a change staged, each once: the staged cells are kept in the order of
        // their table, row and column, so a row's are together.
        let staged = || {
            let mut last: Option<&str> = None;
            app.staged
                .keys()
                .filter(|(of, ..)| of == sheet.name)
                .filter_map(move |(_, pk, _)| {
                    (last != Some(pk.as_str())).then(|| {
                        last = Some(pk.as_str());
                        pk.as_str()
                    })
                })
        };
        let fills = &self.columns.fills;
        let fresh = fills.borrow().as_ref().is_some_and(|fills| {
            let (name, reads, by, rows) = &fills.of;
            (*name, *reads, *by) == (sheet.name, app.reads, filter)
                && rows.iter().map(String::as_str).eq(staged())
        });
        if !fresh {
            // The rows written that the grid shows by the filter: a new row holds nothing the
            // read does.
            let rows: Vec<&SheetRow> = sheet
                .rows
                .iter()
                .filter(|row| !super::rows::is_new(&row.pk) && self.life_shows(app, sheet, row))
                .collect();
            let by = sheet
                .columns
                .iter()
                .map(|column| {
                    let filled = rows
                        .iter()
                        .filter(|row| {
                            row.cells
                                .get(&column.name)
                                .is_some_and(|cell| !cell.text.is_empty())
                        })
                        .count();
                    (column.name.clone(), filled)
                })
                .collect();
            *fills.borrow_mut() = Some(Fills {
                of: (
                    sheet.name,
                    app.reads,
                    filter,
                    staged().map(str::to_owned).collect(),
                ),
                rows: rows.len(),
                by,
            });
        }
        std::cell::Ref::map(fills.borrow(), |fills| {
            fills.as_ref().expect("the fills were counted")
        })
    }

    /// How many of the rows written that the grid shows of `sheet` by its lifecycle filter hold a
    /// value in `column` (the find aside), counted once for each read, filter and set of rows
    /// with a change staged.
    pub(crate) fn fill(&self, app: &TablesApp, sheet: &Sheet, column: &str) -> usize {
        self.fills(app, sheet).by.get(column).copied().unwrap_or(0)
    }

    /// How many rows a fill of `sheet` is counted over: those written that the grid shows by its
    /// lifecycle filter, the find aside.
    pub(crate) fn fill_rows(&self, app: &TablesApp, sheet: &Sheet) -> usize {
        self.fills(app, sheet).rows
    }

    /// What the choice never leaves out of the grid: the key column and a column a cell of which
    /// holds a change not written or a refusal ([`holds`]); and, from the fold (where not
    /// `hide`), the open cell's, the selected cell's, a range's other corner's, the one the rows
    /// are sorted by, whose header says so and is where the sort is undone, and while a new row
    /// of the table stands each column it is typed into, which no written row may fill yet.
    fn kept(&self, app: &TablesApp, sheet: &Sheet, column: &str, hide: bool) -> bool {
        column == sheet.key
            || holds(app, sheet, column).is_some()
            || !hide
                && (self.column.as_deref() == Some(column)
                    || self.editing.as_ref().is_some_and(|e| e.column == column)
                    || self.anchor.as_ref().is_some_and(|c| c.column == column)
                    || self.sorted(sheet).is_some_and(|(by, _)| by.name == column)
                    || app.born.iter().any(|(of, _)| *of == sheet.name)
                        && sheet
                            .life
                            .as_ref()
                            .is_some_and(|life| life.born_columns.iter().any(|c| c == column)))
    }

    /// Whether the grid leaves `column` of `sheet` out, and why.
    fn left_out(&self, app: &TablesApp, sheet: &Sheet, column: &SheetColumn) -> Option<Out> {
        let chosen = self.columns.tables.get(sheet.name);
        if chosen.is_some_and(|c| c.hidden.contains(&column.name))
            && !self.kept(app, sheet, &column.name, true)
        {
            return Some(Out::Hidden);
        }
        let folds = chosen.is_none_or(|c| {
            !c.spread && !c.unfolded.contains(&column.group) && !c.pinned.contains(&column.name)
        });
        // A table whose filter shows no written row has nothing to judge its columns by: its
        // columns stand open, saying what a row would hold.
        (folds
            && self.fill_rows(app, sheet) > 0
            && self.fill(app, sheet, &column.name) == 0
            && !self.kept(app, sheet, &column.name, false))
        .then_some(Out::Folded)
    }

    /// The columns the grid draws of `sheet`, in its order: the key column, then every other the
    /// choice leaves in. Copy, a range, Tab's path, the arrows, a paste and the cell bar's
    /// address take these alone.
    pub(crate) fn shown_columns<'a>(
        &self,
        app: &TablesApp,
        sheet: &'a Sheet,
    ) -> Vec<&'a SheetColumn> {
        ordered(sheet)
            .into_iter()
            .filter(|column| self.left_out(app, sheet, column).is_none())
            .collect()
    }

    /// How many columns of each group the fold leaves out of the grid.
    fn folded(&self, app: &TablesApp, sheet: &Sheet) -> BTreeMap<Group, usize> {
        let mut folded = BTreeMap::new();
        for column in &sheet.columns {
            if self.left_out(app, sheet, column) == Some(Out::Folded) {
                *folded.entry(column.group).or_insert(0) += 1;
            }
        }
        folded
    }

    /// Each folded group's stub, for the grid to draw at the end of its group: a column named
    /// apart from every column of a sheet ([`is_stub`]), headed `N EMPTY`.
    pub(crate) fn stubs(&self, app: &TablesApp, sheet: &Sheet) -> Vec<SheetColumn> {
        self.folded(app, sheet)
            .into_iter()
            .map(|(group, n)| SheetColumn {
                name: format!("{STUB_COLUMN}{}", slug(group)),
                label: format!("{n} EMPTY"),
                group,
                editable: false,
                kind: super::ColumnKind::Text,
                links: None,
                compiled: None,
                key: false,
            })
            .collect()
    }

    /// The grid's columns: those it draws ([`Self::shown_columns`]), each group's stub after its
    /// last column.
    pub(crate) fn laid_columns<'a>(
        &self,
        app: &TablesApp,
        sheet: &'a Sheet,
        stubs: &'a [SheetColumn],
    ) -> Vec<&'a SheetColumn> {
        let all = ordered(sheet);
        let mut laid = Vec::with_capacity(all.len() + stubs.len());
        for (at, column) in all.iter().enumerate() {
            if self.left_out(app, sheet, column).is_none() {
                laid.push(*column);
            }
            // The key column stands first whatever its group: a group's stub follows its last
            // column after it.
            let ends = all
                .get(at + 1)
                .is_none_or(|next| next.group != column.group);
            if at > 0
                && ends
                && let Some(stub) = stubs.iter().find(|stub| stub.group == column.group)
            {
                laid.push(stub);
            }
        }
        laid
    }

    /// Where the selected cell's column is one the grid does not draw (a person hid it), the
    /// selection moves to the nearest column it draws in the same row: the next one, else the
    /// one before. The keys go with it where they were on the cell; a range whose other corner
    /// stood in it is the one selected cell again. Where the grid has no place yet for the
    /// selected column (a folded one a cell chosen from elsewhere stands in, or another table's
    /// columns were laid out last), it is placed as the grid will lay it out and scrolled into
    /// view. Whether anything moved.
    pub(crate) fn settle_columns(&mut self, app: &TablesApp) -> bool {
        let Some(sheet) = app.sheet(self.sheet) else {
            return false;
        };
        let was = (
            self.column.clone(),
            self.anchor.is_some(),
            self.across,
            self.common.last_focus.clone(),
        );
        let shown: Vec<String> = self
            .shown_columns(app, sheet)
            .iter()
            .map(|c| c.name.clone())
            .collect();
        let all: Vec<&str> = ordered(sheet).iter().map(|c| c.name.as_str()).collect();
        if let Some(column) = self.column.clone()
            && !shown.contains(&column)
            && let Some(at) = all.iter().position(|name| *name == column)
        {
            let drawn = |name: &&&str| shown.iter().any(|s| s == **name);
            let near = all[at..]
                .iter()
                .find(drawn)
                .or_else(|| all[..at].iter().rev().find(drawn));
            if let Some(near) = near.map(|name| (*name).to_owned()) {
                let was = self.row.as_ref().map(|pk| cell_key(pk, &column));
                let keys_on = self
                    .common
                    .last_focus
                    .as_ref()
                    .is_some_and(|focus| Some(focus.as_str()) == was.as_deref());
                self.column = Some(near.clone());
                if keys_on && let Some(pk) = self.row.clone() {
                    self.focus(&cell_key(&pk, &near));
                }
                self.reveal(&near);
            }
        }
        if self
            .anchor
            .as_ref()
            .is_some_and(|corner| !shown.contains(&corner.column))
        {
            self.anchor = None;
        }
        if let Some(column) = self.column.clone()
            && column != sheet.key
            && shown.contains(&column)
            && (self.columns.laid.get() != Some(sheet.name)
                || !self.spans.borrow().iter().any(|(name, ..)| *name == column))
        {
            self.reveal_laid(app, sheet, &column);
        }
        was != (
            self.column.clone(),
            self.anchor.is_some(),
            self.across,
            self.common.last_focus.clone(),
        )
    }

    /// Show `column` of `sheet` in the grid as its line in the list shows it: hidden no longer,
    /// and, where no row fills it, pinned, so the fold leaves it drawn after the selection leaves
    /// it while the group's other empty columns stay folded. A press elsewhere that names one of
    /// its cells (a line of the history drawer) asks to see that cell; [`Self::settle_columns`]
    /// then scrolls across to it where the grid had no place for it.
    pub(crate) fn show_column(&mut self, app: &TablesApp, sheet: &Sheet, column: &str) {
        if column == sheet.key {
            return;
        }
        let folds = self.fill(app, sheet, column) == 0;
        let chosen = self.columns.chosen_mut(sheet.name);
        chosen.hidden.remove(column);
        if folds {
            chosen.pinned.insert(column.to_owned());
        }
    }

    /// Scroll across so `column` of `sheet` shows whole, its place worked out as [`super::grid`]
    /// will lay it out: a column the grid did not lay out last has no place there yet.
    fn reveal_laid(&mut self, app: &TablesApp, sheet: &Sheet, column: &str) {
        let stubs = self.stubs(app, sheet);
        let columns = self.laid_columns(app, sheet, &stubs);
        let widths = widths(app, self, sheet, &columns);
        let mut spans = Vec::with_capacity(columns.len());
        let mut at = 0.0;
        for (laid, w) in columns.iter().zip(&widths).skip(1) {
            spans.push((laid.name.clone(), at, at + w));
            at += w;
        }
        let place = spans
            .iter()
            .find(|(name, ..)| name == column)
            .map(|(_, start, end)| (*start, *end));
        *self.spans.borrow_mut() = spans;
        self.columns.laid.set(Some(sheet.name));
        let room = self.grid_room.get();
        if let Some((start, end)) = place
            && room > 0.0
        {
            self.across = across_for(self.across, start, end, room);
        }
    }

    /// Open the list or close it; open, the keys go to its first line, closed, back to the
    /// Columns button.
    fn open_columns(&mut self, open: bool) {
        self.columns.open = open;
        if open {
            self.columns.scroll = 0.0;
            self.focus_line(FOLD);
        } else {
            self.focus(COLUMNS);
        }
    }

    /// Put the keys on the list's line `key`.
    fn focus_line(&mut self, key: &str) {
        self.columns.line = Some(key.to_owned());
        self.focus(key);
    }

    /// The open list's buttons of `sheet`, top down, each with whether it takes a press: the
    /// switch; each column but the key, group by group in the grid's order (one holding a
    /// change not written or a refused cell takes none); Show all, where anything is hidden or
    /// folds.
    fn list_keys(&self, app: &TablesApp, sheet: &Sheet) -> Vec<(String, bool)> {
        let mut keys = vec![(FOLD.to_owned(), true)];
        for group_of in Group::ALL {
            for column in ordered(sheet)
                .into_iter()
                .filter(|c| c.group == group_of && c.name != sheet.key)
            {
                keys.push((
                    format!("{COLUMN}{}", column.name),
                    !self.kept(app, sheet, &column.name, true),
                ));
            }
        }
        let folding = self
            .columns
            .tables
            .get(sheet.name)
            .is_none_or(|c| !c.spread);
        let all_takes = folding || self.shown_columns(app, sheet).len() < sheet.columns.len();
        keys.push((ALL.to_owned(), all_takes));
        keys
    }

    /// Up (`-1`) or Down (`1`) from the list's line with the keys, to the nearest one above or
    /// below that takes a press, scrolled into view where the list was laid out: from a line
    /// that takes none (Show all once it has shown everything) as from any other, from where it
    /// stands; past either end, nowhere; from the Columns button, to the first.
    fn step_columns(&mut self, app: &TablesApp, by: i32) {
        let Some(sheet) = app.current(self).and_then(|name| app.sheet(name)) else {
            return;
        };
        let keys = self.list_keys(app, sheet);
        let takes = |at: usize| keys[at].1;
        let at = self
            .common
            .last_focus
            .as_ref()
            .and_then(|focus| keys.iter().position(|(key, _)| key == focus.as_str()));
        let to = match at {
            None => (0..keys.len()).find(|at| takes(*at)),
            Some(at) if by < 0 => (0..at).rev().find(|at| takes(*at)),
            Some(at) => (at + 1..keys.len()).find(|at| takes(*at)),
        };
        let Some(to) = to else {
            return;
        };
        let key = &keys[to].0;
        let line = self
            .columns
            .lines
            .borrow()
            .iter()
            .find(|line| line.0 == *key)
            .map(|line| (line.1, line.2));
        if let Some((top, bottom)) = line {
            self.reveal_column_line(top, bottom);
        }
        let key = key.clone();
        self.focus_line(&key);
        self.common.focus_visible = true;
    }

    /// Scroll a line of the open list, or a folded group's stub, named by `key` into view, as
    /// [`TablesViewState::reveal_listed_control`] does the grid's controls: whether anything
    /// moved; `None` for a key of neither.
    pub(crate) fn reveal_columns_control(&mut self, key: &str) -> Option<bool> {
        if let Some(word) = key.strip_prefix(STUB) {
            let across = self.across;
            self.reveal(&format!("{STUB_COLUMN}{word}"));
            return Some(across != self.across);
        }
        if !key.starts_with(LIST) {
            return None;
        }
        let line = self
            .columns
            .lines
            .borrow()
            .iter()
            .find(|line| line.0 == key)
            .map(|line| (line.1, line.2));
        let was = self.columns.scroll;
        if let Some((top, bottom)) = line {
            self.reveal_column_line(top, bottom);
        }
        Some(was != self.columns.scroll)
    }

    /// Scroll the list so the line from `top` to `bottom` shows whole.
    fn reveal_column_line(&mut self, top: f32, bottom: f32) {
        let room = self.columns.room.get();
        let scroll = &mut self.columns.scroll;
        if top < *scroll {
            *scroll = top;
        } else if room > 0.0 && bottom > *scroll + room {
            *scroll = bottom - room;
        }
        *scroll = scroll.clamp(0.0, self.columns.limit.get());
    }
}

/// The Columns control's share of the page's reducer ([`TablesApp::act`]): `Some` where it took
/// the event whole, with whether anything changed; `None` where the page goes on with it. A
/// press elsewhere, or the keys leaving for another control, closes the list first; the page
/// repaints for the list closed whatever the page made of the event.
#[expect(
    clippy::too_many_lines,
    reason = "one reducer, each arm a part of the list"
)]
pub(crate) fn act(
    app: &mut TablesApp,
    view: &mut TablesViewState,
    event: &AppEvent,
) -> Option<bool> {
    let on_list = |view: &TablesViewState| {
        view.common
            .last_focus
            .as_ref()
            .is_some_and(|key| key.as_str().starts_with(LIST) || key.as_str() == COLUMNS)
    };
    match event {
        AppEvent::Action(action) => {
            let key = action.id.as_str();
            if key == COLUMNS {
                view.close_edit(app, false);
                view.open_columns(!view.columns.open);
                return Some(true);
            }
            if let Some(word) = key.strip_prefix(STUB) {
                let group = Group::ALL.into_iter().find(|group| slug(*group) == word)?;
                let name = app.current(view)?;
                let sheet = app.sheet(name)?;
                let keyboard = view.keyboard_on(key);
                // The first of the group's columns the fold leaves out, which the stub shows.
                let first = ordered(sheet)
                    .into_iter()
                    .find(|c| c.group == group && view.left_out(app, sheet, c) == Some(Out::Folded))
                    .map(|c| c.name.clone());
                view.columns.chosen_mut(name).unfolded.insert(group);
                view.columns.open = false;
                // The stub goes with the fold: keys left on it would reach the page's default
                // button (Write) at Return. Pressed from the keyboard they go to the header of
                // the first column it showed; after a pointer's press, to the selected cell, as
                // a header's press leaves them.
                match (keyboard, view.row.clone(), view.column.clone(), first) {
                    (false, Some(pk), Some(column), _) => view.focus(&cell_key(&pk, &column)),
                    (_, _, _, Some(first)) => {
                        view.focus(&format!("{HEAD}{first}"));
                        view.reveal_laid(app, sheet, &first);
                    }
                    _ => view.focus(COLUMNS),
                }
                return Some(true);
            }
            if key.starts_with(LIST) {
                let name = app.current(view)?;
                let sheet = app.sheet(name)?;
                match key {
                    // A press on the list's words or its frame is the list's, and leaves the keys
                    // on the line they were on.
                    PANEL => {
                        let line = view.columns.line.clone();
                        view.focus(line.as_deref().unwrap_or(FOLD));
                        return Some(true);
                    }
                    FOLD => {
                        let chosen = view.columns.chosen_mut(name);
                        chosen.spread = !chosen.spread;
                        // On again, it folds every group's empty columns again.
                        if !chosen.spread {
                            chosen.unfolded.clear();
                            chosen.pinned.clear();
                        }
                    }
                    ALL => {
                        let chosen = view.columns.chosen_mut(name);
                        chosen.hidden.clear();
                        chosen.spread = true;
                    }
                    _ => {
                        let column = key.strip_prefix(COLUMN)?;
                        let found = sheet.columns.iter().find(|c| c.name == column)?;
                        if found.name == sheet.key || view.kept(app, sheet, column, true) {
                            return Some(false);
                        }
                        let drawn = view.left_out(app, sheet, found).is_none();
                        let folds = view.fill(app, sheet, column) == 0;
                        let chosen = view.columns.chosen_mut(name);
                        if drawn {
                            chosen.hidden.insert(column.to_owned());
                            chosen.pinned.remove(column);
                        } else {
                            chosen.hidden.remove(column);
                            if folds {
                                chosen.pinned.insert(column.to_owned());
                            }
                        }
                        // Hidden, the column takes its header with it, which says the rows are
                        // sorted by it and is where the sort is undone: so the sort goes too,
                        // the selected row kept in view, as a third press on the header does.
                        if drawn && view.sort.get(name).is_some_and(|(by, _)| by == column) {
                            view.sort.remove(name);
                            let at = view.row.as_ref().and_then(|pk| {
                                view.shown_rows(app, sheet)
                                    .iter()
                                    .position(|row| row.pk == *pk)
                            });
                            match at {
                                Some(at) => view.keep_in_view(at),
                                None => view.scroll.set(0.0),
                            }
                        }
                    }
                }
                view.focus_line(key);
                return Some(true);
            }
            // A press anywhere else closes the list; the page takes the press. Where the keys
            // were on the list (an action no press made: a paste, an assistive technology's)
            // they go back to the Columns button, which the page still draws: keys on a line it
            // no longer draws would reach its default button, Write, at Return.
            if view.columns.open
                && view
                    .common
                    .last_focus
                    .as_ref()
                    .is_some_and(|focus| focus.as_str().starts_with(LIST))
            {
                view.focus(COLUMNS);
            }
            view.columns.open = false;
            None
        }
        AppEvent::FocusChanged(focus) if view.columns.open => {
            let stays = focus
                .as_ref()
                .is_some_and(|key| key.as_str().starts_with(LIST) || key.as_str() == COLUMNS);
            if !stays {
                view.columns.open = false;
            }
            None
        }
        AppEvent::TextInput(crate::native_app::TextInputEvent::Cancel)
            if view.columns.open && on_list(view) =>
        {
            view.open_columns(false);
            Some(true)
        }
        // The list keeps the keys while it holds them: they move among its lines.
        AppEvent::TextInput(_) if view.columns.open && on_list(view) => Some(true),
        AppEvent::StepFocus(by) if view.columns.open && on_list(view) => {
            view.step_columns(app, *by);
            Some(true)
        }
        AppEvent::ScrollAt { lines, key } if key.as_str().starts_with(LIST) => {
            view.columns.scroll = (view.columns.scroll + *lines as f32 * row_height())
                .clamp(0.0, view.columns.limit.get());
            Some(true)
        }
        _ => None,
    }
}

/// A press the size of its parent under painted words, `spoken` its name: one that takes no
/// press where not `enabled`.
pub(crate) fn line_press(key: &str, spoken: String, enabled: bool) -> UiNode {
    UiNode::new(
        key.to_owned(),
        UiContent::Button(
            Control::new(
                ButtonSpec::new(spoken).visual_label(String::new()),
                ActionId::new(key.to_owned()),
            )
            .state(ControlState {
                enabled,
                ..ControlState::default()
            })
            .style(StyleRef::Navigation),
        ),
    )
}

/// A column header's fill: a bar along its lower edge, as long across the cell `width` wide as
/// the share of `rows` that fill it. Painted only: the header's press says it in its name.
pub(crate) fn fill_bar(key: &str, fill: usize, rows: usize, width: f32) -> UiNode {
    let share = if rows == 0 {
        0.0
    } else {
        fill.min(rows) as f32 / rows as f32
    };
    group(format!("{key}/fill"))
        .layout(Layout::column().padding(Insets {
            bottom: 1.0,
            ..Insets::default()
        }))
        .children(vec![
            group(format!("{key}/fill/space")).layout(Layout::default().height(Length::Fill)),
            group(format!("{key}/fill/line"))
                .layout(Layout::row().height(Length::Fixed(FILL_BAR)))
                .children(vec![
                    styled(format!("{key}/fill/bar"), StyleRef::SheetBar).layout(
                        Layout::default().width(Length::Fixed(((width - 1.0) * share).max(0.0))),
                    ),
                ]),
        ])
        .paint_only_tree()
}

/// A stub's seat in the header line: `N EMPTY`, a press that unfolds its group.
pub(crate) fn stub_head(column: &SheetColumn, width: f32) -> UiNode {
    let word = &column.name[STUB_COLUMN.len()..];
    let key = format!("{STUB}{word}");
    let n: usize = column
        .label
        .split(' ')
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    let spoken = match n {
        1 => format!(
            "1 empty column folded under {}; show it",
            group_words(column.group)
        ),
        n => format!(
            "{n} empty columns folded under {}; show them",
            group_words(column.group)
        ),
    };
    head_cell(
        format!("{key}/seat"),
        width,
        vec![
            press(&key, spoken),
            group(format!("{key}/face"))
                .layout(Layout::row().padding(Insets::symmetric(10.0, 0.0)))
                .children(vec![fitted(
                    format!("{key}/word"),
                    &column.label,
                    StyleRef::Note,
                    (width - 20.0).max(1.0),
                )])
                .paint_only_tree(),
        ],
    )
}

/// A stub's seat on row `pk`: a ruled cell `width` wide and `line` tall, blank, which takes no
/// press.
pub(crate) fn stub_cell(column: &SheetColumn, pk: &str, width: f32, line: f32) -> UiNode {
    let word = &column.name[STUB_COLUMN.len()..];
    styled(format!("{STUB}{word}/row/{pk}"), StyleRef::Cell).layout(
        Layout::default()
            .width(Length::Fixed(width))
            .height(Length::Fixed(line)),
    )
}

/// The Columns button's label: `Columns 15 of 21`, the columns the grid draws of all the
/// table's, or (`short`) `Columns` alone; and how many it draws of how many.
fn control_label(
    app: &TablesApp,
    view: &TablesViewState,
    sheet: &Sheet,
    short: bool,
) -> (String, usize, usize) {
    let shown = view.shown_columns(app, sheet).len();
    let all = sheet.columns.len();
    let label = if short {
        "Columns".to_owned()
    } else {
        format!("Columns {shown} of {all}")
    };
    (label, shown, all)
}

/// How wide the Columns button is whole ([`control`]).
pub(crate) fn control_width(
    app: &TablesApp,
    view: &TablesViewState,
    sheet: &Sheet,
    short: bool,
) -> f32 {
    crate::native_ui::native_button_label_width(&control_label(app, view, sheet, short).0) + 24.0
}

/// The tools row's Columns button, `width` wide: `Columns 15 of 21`, the columns the grid draws
/// of all the table's, or (`short`, where the row has no room for the count) `Columns` alone,
/// its name still saying the count. Narrower than its label, the label is cut.
pub(crate) fn control(
    app: &TablesApp,
    view: &TablesViewState,
    sheet: &Sheet,
    short: bool,
    width: f32,
) -> UiNode {
    let (label, shown, all) = control_label(app, view, sheet, short);
    let spoken = if view.columns.open {
        format!("Columns {shown} of {all} shown; close the list of columns")
    } else {
        format!("Columns {shown} of {all} shown; choose the columns shown")
    };
    let button = UiNode::new(
        COLUMNS,
        UiContent::Button(
            Control::new(
                ButtonSpec::new(spoken).visual_label(label),
                ActionId::new(COLUMNS),
            )
            .state(ControlState {
                enabled: true,
                expanded: Some(view.columns.open),
                ..ControlState::default()
            })
            .style(StyleRef::Secondary),
        ),
    )
    .layout(
        Layout::default()
            .width(Length::Fixed(width))
            .height(Length::Fill),
    );
    group("tables/columns-seat")
        .layout(
            Layout::row()
                .width(Length::Fixed(width))
                .padding(Insets::symmetric(0.0, 3.0)),
        )
        .children(vec![button])
}

/// One line of the open list: its press over its painted words, the keys' ring where they are
/// on it.
fn list_line(
    view: &TablesViewState,
    key: &str,
    spoken: String,
    enabled: bool,
    face: Vec<UiNode>,
    (width, line): (f32, f32),
) -> UiNode {
    let mut seat = vec![
        line_press(key, spoken, enabled),
        group(format!("{key}/face"))
            .layout(
                Layout::row()
                    .gap(LINE_GAP)
                    .padding(Insets::symmetric(10.0, 0.0)),
            )
            .children(face)
            .paint_only_tree(),
    ];
    let ringed = view.common.focus_visible
        && view
            .common
            .last_focus
            .as_ref()
            .is_some_and(|focus| focus.as_str() == key);
    if ringed {
        seat.push(styled(format!("{key}/ring"), StyleRef::CellSelected).paint_only());
    }
    group(format!("{key}/seat"))
        .layout(
            Layout::default()
                .width(Length::Fixed(width))
                .height(Length::Fixed(line)),
        )
        .children(seat)
}

/// The open list of `sheet`'s columns, laid over the page's main column: its right edge at
/// `right` and its top at `top`, no lower than `foot`. Where its lines are taller than that
/// room they scroll under its heading.
#[expect(clippy::too_many_lines, reason = "one list, laid out in one pass")]
pub(crate) fn list(
    app: &TablesApp,
    view: &TablesViewState,
    sheet: &Sheet,
    (right, top, foot): (f32, f32, f32),
) -> UiNode {
    let line = row_height();
    let caption = 22.0 * scale().max(1.0);
    let width = (LIST_WIDTH * scale().max(1.0)).min(right).max(1.0);
    // The words' room on a line: the ground's width (the frame's left rule off the list's) less
    // the line's padding at each end.
    let inner = width - 1.0 - 20.0;
    let rows = view.fill_rows(app, sheet);
    let shown = view.shown_columns(app, sheet);
    let folded: usize = view.folded(app, sheet).values().sum();
    let chosen = view.columns.tables.get(sheet.name);
    let folding = chosen.is_none_or(|c| !c.spread);
    let mark_w = text_width("\u{2713}", SemanticRole::Text, StyleRef::Primary);
    // A line's words sit `LINE_GAP` apart: what a row of `n` of them gives its gaps.
    let gaps = |n: usize| LINE_GAP * n.saturating_sub(1) as f32;
    let mark = |key: &str, on: bool| {
        fitted(
            format!("{key}/mark"),
            if on { "\u{2713}" } else { "" },
            StyleRef::Primary,
            mark_w,
        )
    };
    let mut lines = Vec::new();
    let mut body = Vec::new();
    let mut tall = 0.0_f32;
    // The switch.
    {
        let state = if folding {
            format!("{folded} folded")
        } else {
            "off".to_owned()
        };
        let state_w = text_width(&state, SemanticRole::Text, StyleRef::Note);
        let spoken = if folding {
            format!("Fold columns no row fills, on, {folded} folded")
        } else {
            "Fold columns no row fills, off".to_owned()
        };
        body.push(list_line(
            view,
            FOLD,
            spoken,
            true,
            vec![
                mark(FOLD, folding),
                fitted(
                    format!("{FOLD}/words"),
                    "Fold columns no row fills",
                    StyleRef::Primary,
                    (inner - mark_w - state_w - gaps(4)).max(1.0),
                ),
                group(format!("{FOLD}/lead")).layout(Layout::default().width(Length::Fill)),
                fitted(format!("{FOLD}/state"), &state, StyleRef::Note, state_w),
            ],
            (width, line),
        ));
        lines.push((FOLD.to_owned(), tall, tall + line));
        tall += line;
    }
    // Each group's columns under its word, the key column not among them.
    let mut first = true;
    for group_of in Group::ALL {
        let offered: Vec<&SheetColumn> = ordered(sheet)
            .into_iter()
            .filter(|c| c.group == group_of && c.name != sheet.key)
            .collect();
        if offered.is_empty() {
            continue;
        }
        let word_key = format!("{LIST}group/{}", slug(group_of));
        let mut caption_line = vec![fitted(
            format!("{word_key}/word"),
            group_of.word(),
            StyleRef::Note,
            (inner * 0.5).max(1.0),
        )];
        if first {
            caption_line.push(
                group(format!("{word_key}/lead")).layout(Layout::default().width(Length::Fill)),
            );
            // Its own width, so the space before it sets it over the counts.
            let note = "rows with a value";
            let note_w = text_width(note, SemanticRole::Text, StyleRef::Note);
            caption_line.push(fitted(
                format!("{word_key}/note"),
                note,
                StyleRef::Note,
                note_w.min(inner * 0.5).max(1.0),
            ));
            first = false;
        }
        body.push(
            group(word_key)
                .layout(
                    Layout::row()
                        .width(Length::Fixed(width))
                        .height(Length::Fixed(caption))
                        .padding(Insets {
                            left: 10.0,
                            right: 10.0,
                            top: 6.0,
                            bottom: 0.0,
                        }),
                )
                .children(caption_line),
        );
        tall += caption;
        for column in offered {
            let key = format!("{COLUMN}{}", column.name);
            let fill = view.fill(app, sheet, &column.name);
            let drawn = shown.iter().any(|c| c.name == column.name);
            let held = holds(app, sheet, &column.name);
            let state = if drawn {
                "shown"
            } else if view.left_out(app, sheet, column) == Some(Out::Folded) {
                "folded"
            } else {
                "hidden"
            };
            let name = field_words(column);
            let note = held.map(Holds::note);
            let mut spoken = format!("{name}, {state}");
            if let Some((note, _)) = note {
                spoken.push_str(&format!(", {note}"));
            }
            spoken.push_str(&format!(", {}", fill_words(fill, rows)));
            let count = format!("{fill} of {rows}");
            let count_w = text_width(&count, SemanticRole::Text, StyleRef::Note);
            let note_w = note.map_or(0.0, |(note, ink)| text_width(note, SemanticRole::Text, ink));
            // The mark, the name, the note where it has one, the space and the count.
            let words = if note.is_some() { 5 } else { 4 };
            let name_w = (inner - mark_w - count_w - note_w - gaps(words)).max(1.0);
            // A column no row fills has its name in the note's ink, which reads on the list's
            // ground as every word of the sheet does.
            let mut face = vec![
                mark(&key, drawn),
                fitted(
                    format!("{key}/name"),
                    &name,
                    if fill == 0 {
                        StyleRef::Note
                    } else {
                        StyleRef::Primary
                    },
                    name_w,
                ),
            ];
            if let Some((note, ink)) = note {
                face.push(fitted(format!("{key}/note"), note, ink, note_w));
            }
            face.push(group(format!("{key}/lead")).layout(Layout::default().width(Length::Fill)));
            face.push(fitted(
                format!("{key}/fill"),
                &count,
                StyleRef::Note,
                count_w,
            ));
            body.push(list_line(
                view,
                &key,
                spoken,
                held.is_none(),
                face,
                (width, line),
            ));
            lines.push((key, tall, tall + line));
            tall += line;
        }
    }
    // Show all, and what the list writes.
    let all_takes = shown.len() < sheet.columns.len() || folding;
    body.push(list_line(
        view,
        ALL,
        "Show all columns, and stop folding the empty ones".to_owned(),
        all_takes,
        vec![
            mark(ALL, false),
            fitted(
                format!("{ALL}/words"),
                "Show all",
                if all_takes {
                    StyleRef::Primary
                } else {
                    StyleRef::Note
                },
                (inner - mark_w - gaps(2)).max(1.0),
            ),
        ],
        (width, line),
    ));
    lines.push((ALL.to_owned(), tall, tall + line));
    tall += line;
    body.push(
        group(format!("{LIST}note/seat"))
            .layout(
                Layout::row()
                    .width(Length::Fixed(width))
                    .height(Length::Fixed(caption + 6.0))
                    .padding(Insets {
                        left: 10.0,
                        right: 10.0,
                        top: 4.0,
                        bottom: 2.0,
                    }),
            )
            .children(vec![fitted(
                format!("{LIST}note"),
                "What you show here is this view's. It writes nothing.",
                StyleRef::Note,
                inner,
            )]),
    );
    tall += caption + 6.0;
    *view.columns.lines.borrow_mut() = lines;
    // The heading stays; the lines under it scroll where the room is short of them.
    let head_h = line;
    let room = (foot - top - head_h - 2.0).max(line);
    let body_h = tall.min(room);
    let limit = (tall - body_h).max(0.0);
    view.columns.room.set(body_h);
    view.columns.limit.set(limit);
    let scroll = view.columns.scroll.min(limit);
    let counted = format!("{} of {} shown", shown.len(), sheet.columns.len());
    let counted_w = text_width(&counted, SemanticRole::Text, StyleRef::Note);
    let heading = group(format!("{LIST}head"))
        .layout(
            Layout::row()
                .width(Length::Fixed(width))
                .height(Length::Fixed(head_h))
                .padding(Insets::symmetric(10.0, 0.0)),
        )
        .children(vec![
            UiNode::new(
                format!("{LIST}head/title"),
                UiContent::Text(crate::native_ui::TextSpec {
                    text: "Columns".into(),
                    role: SemanticRole::Heading,
                    style: StyleRef::Primary,
                }),
            )
            .layout(Layout::default().width(Length::Fill)),
            fitted(
                format!("{LIST}head/count"),
                &counted,
                StyleRef::Note,
                counted_w,
            ),
        ]);
    let lines_node = group(format!("{LIST}lines"))
        .layout(
            Layout::column()
                .width(Length::Fixed(width))
                .height(Length::Fixed(body_h))
                .clipped()
                .scrolled(scroll),
        )
        .room(body_h, limit > 0.0)
        .lists_every_row()
        .children(body);
    let height = head_h + body_h + 2.0;
    // Ruled round, on the sheet's raised ground: the frame's top and left rules, then the
    // ground, which rules its own right and bottom. The whole of it is a surface over the page:
    // a press on its words or its frame, and the wheel over it, are the list's.
    let panel = UiNode::new(PANEL, UiContent::Group(GroupSpec::new("List of columns")))
        .surface(ActionId::new(PANEL))
        .layout(
            Layout::column()
                .width(Length::Fixed(width))
                .height(Length::Fixed(height)),
        )
        .children(vec![
            styled(format!("{LIST}panel/top"), StyleRef::Cell)
                .layout(Layout::default().height(Length::Fixed(1.0))),
            group(format!("{LIST}panel/sides"))
                .layout(Layout::row())
                .children(vec![
                    styled(format!("{LIST}panel/left"), StyleRef::Cell)
                        .layout(Layout::default().width(Length::Fixed(1.0))),
                    styled(format!("{LIST}panel/ground"), StyleRef::CellHead)
                        .layout(Layout::column())
                        .children(vec![heading, lines_node]),
                ]),
        ]);
    group(format!("{LIST}over"))
        .layout(Layout::column())
        .children(vec![
            group(format!("{LIST}above"))
                .layout(Layout::default().height(Length::Fixed(top.max(0.0)))),
            group(format!("{LIST}line"))
                .layout(Layout::row().height(Length::Fixed(height)))
                .children(vec![
                    group(format!("{LIST}before"))
                        .layout(Layout::default().width(Length::Fixed((right - width).max(0.0)))),
                    panel,
                ]),
        ])
}

/// The page's main column `main`, `width` wide, with `list` laid over it.
pub(crate) fn over(main: UiNode, list: UiNode, width: f32) -> UiNode {
    group("tables/main-over")
        .layout(Layout::default().width(Length::Fixed(width)))
        .children(vec![main, list])
}

#[cfg(all(test, unix))]
#[path = "native_tables_columns_tests.rs"]
mod native_tables_columns_tests;
