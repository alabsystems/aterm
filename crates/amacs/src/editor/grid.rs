// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The grid face of an editor view: a CSV or TSV document edited cell by cell
//! (`docs/DESIGN-amacs-grid-2026-10-28.md`, slices G1 and G2).
//!
//! The grid keeps no copy of the data. The current cell is the one holding the
//! view's primary caret, so every writer's rebase keeps it in place; a commit
//! is one [`EditorWorkspace::apply_recorded`] transaction over the cell's own
//! bytes, so the grid and the text view share one undo history. A row added or
//! deleted is one transaction over that row's own bytes too, and the kills,
//! copy and yank move a cell's value (never its quoted bytes) through the kill
//! ring.

use std::ops::Range;
use std::sync::Arc;

use aterm_grapheme::GraphemeClusters;
use aterm_types::keyboard::{Key, Modifiers, NamedKey};

use super::chord::KeyChord;
use super::command::{EditorCommand, PrefixArgument};
use super::keymap::Profile;
use super::projection::{editor_display_column, line_start, line_text_end};
use super::view::{
    EditorBufferView, Selection, next_boundary, next_line_position, next_word_boundary,
    previous_boundary, previous_line_position, previous_word_boundary,
};
use super::workspace::{EditorEffect, EditorError, EditorWorkspace, snapshot_for_view};
use crate::input::{EditorInput, Reduction};
use crate::language::Language;
use crate::store::{Concealed, DocumentSnapshot, DocumentStore, EditDelta, rebase_position};
use crate::table::{self, Delimiter, Table, TooLarge, Unwritable};

/// What a key the open cell editor does not take says.
pub const DRAFT_HOLDS: &str = "Return keeps the cell, Escape drops it";

/// What a commit says when another writer changed the cell under the draft.
pub const CELL_CHANGED: &str = "the cell changed while you edited it; Escape drops your text";

/// What `toggle-grid-view` says in the terminal, which shows a table as text.
pub const TERMINAL_TABLE: &str =
    "The terminal editor shows a table as text; an aterm Editor tab shows it as a grid";

/// What a commit says when the cell lies past a quote that never closes.
pub const ROW_UNCLOSED: &str = "this row's quote never closes, so the cell cannot be added; fix it in the text view (C-c C-t or Cmd-E)";

/// What opening, clearing, pasting into or committing a cell says when its own quote never
/// closes: it holds every later row, so writing it would drop them.
pub const CELL_UNCLOSED: &str = "this cell's quote never closes, so it holds every row after it and the grid does not change it; fix it in the text view (C-c C-t or Cmd-E)";

/// What adding a row after, or deleting, a row whose quote never closes says: the quote holds
/// every row after it, so neither would leave the file as the grid shows it.
pub const ROW_HOLDS_REST: &str = "this row's quote never closes, so it holds every row after it and the grid does not add or delete rows there; fix it in the text view (C-c C-t or Cmd-E)";

/// What deleting a row says when the lone `\r` above it and the blank `\n` line below it would
/// read as one `\r\n` ([`Unwritable::JoinsLineBreaks`]).
pub const ROW_JOINS_BREAKS: &str = "deleting this row would join the line break above it and the blank line below it into one, dropping that blank line too; delete it in the text view (C-c C-t or Cmd-E)";

/// What a copy or a cut of a cell with no value says.
pub const EMPTY_CELL: &str = "The cell is empty: nothing to copy or cut";

/// What a copy of a concealed cell says ([`Concealed`]): a status is read out to the window's
/// readers, so it names no value.
pub const COPIED_HIDDEN: &str = "Copied the hidden value";

/// What the window's readers other than the person's eyes get in place of a concealed cell's
/// value ([`Concealed`]; one-mesh design §4.8).
pub const HIDDEN_VALUE: &str = "hidden value";

/// Why a kill or copy of a concealed document's text was refused to input without clipboard
/// authority ([`Concealed`]).
pub const CONCEALED_STAYS: &str =
    "A hidden value is copied or cut only on the person's own keystroke";

/// The words for why `edit_cell` wrote nothing.
const fn unwritable(why: Unwritable) -> &'static str {
    match why {
        Unwritable::UnclosedCell => CELL_UNCLOSED,
        Unwritable::UnclosedRow => ROW_UNCLOSED,
        Unwritable::JoinsLineBreaks => ROW_JOINS_BREAKS,
    }
}

/// How a view draws its document.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Face {
    /// Lines of text.
    #[default]
    Text,
    /// Records and cells (`.csv`, `.tsv`).
    Grid(GridState),
}

/// The grid face's own state. The current cell's row is the record holding the view's primary
/// caret; `column` names its column, which a missing cell of a short row needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GridState {
    pub delimiter: Delimiter,
    /// The current cell's column.
    pub column: usize,
    /// The first record under the pinned header (at least 1).
    pub top_record: usize,
    /// The first column the grid tries to show; the projection moves past it to show the cursor.
    /// While it is past the first column, the first column stays pinned at the left edge.
    pub left_column: usize,
    /// Whether a sideways scroll ([`EditorWorkspace::scroll_grid_columns`]) put `left_column`
    /// where it is: the grid then shows from it even when the cursor's column is off to the side,
    /// until the cursor next moves.
    pub scrolled: bool,
    /// How many character cells a row of the face is wide ([`EditorBufferView::set_grid_width`]),
    /// so a move keeps `left_column` where the face shows it; 0 until a face says.
    pub cells_wide: usize,
    /// The open cell editor, if any.
    pub draft: Option<CellDraft>,
}

/// An open cell editor: the cell it is tied to, the value it opened with, and the text being typed.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CellDraft {
    pub column: usize,
    /// The bytes of the cell's record (its terminator left out) as the draft last saw them,
    /// carried through every other writer's commit ([`Self::observe`]): the draft's cell is
    /// `column` of the record that starts here, whatever happens to the caret.
    pub record: Range<usize>,
    /// Whether another writer's commit touched the record, or met it at either end, since the
    /// draft opened or was last confirmed: a commit then asks the person to look again.
    pub moved: bool,
    /// The cell's value when the draft opened; a commit lands only while the cell still holds it.
    pub original: String,
    pub text: String,
    /// The caret's byte in `text`.
    pub caret: usize,
    /// The draft's own history: the text and caret before each run of typing or deleting.
    pub undo: Vec<(String, usize)>,
    /// What an undo took back, for redo; any new edit clears it.
    pub redo: Vec<(String, usize)>,
    /// The kind of the run of edits the last step belongs to, so typing a word is one undo.
    run: Option<DraftRun>,
}

/// A run of like edits in a draft, undone together.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum DraftRun {
    Insert,
    Delete,
}

impl CellDraft {
    /// Carry the draft's record through a commit another writer made. A change wholly before the
    /// record moves it; one wholly after leaves it; one that touches it, or meets it at either
    /// end (a row added there, a column appended, the row deleted or replaced), marks the draft
    /// [`Self::moved`], since which cell is the draft's own is then a guess.
    pub fn observe(&mut self, deltas: &[EditDelta]) {
        let span = self.record.clone();
        if deltas
            .iter()
            .any(|delta| delta.old.start <= span.end && span.start <= delta.old.end)
        {
            self.moved = true;
        }
        let start = rebase_position(span.start, deltas);
        let end = rebase_position(span.end, deltas).max(start);
        self.record = start..end;
    }

    /// Note an edit of `kind` about to change the text: a new run saves the text and caret for
    /// undo, and any edit ends what redo could bring back.
    fn begin(&mut self, kind: DraftRun) {
        if self.run != Some(kind) {
            self.undo.push((self.text.clone(), self.caret));
            self.run = Some(kind);
        }
        self.redo.clear();
    }

    /// End the current run of edits, as a caret move does.
    fn end_run(&mut self) {
        self.run = None;
    }

    /// Step the draft's own history back (`undo`) or forward; nothing when there is no step.
    fn step_history(&mut self, undo: bool) {
        let (from, to) = if undo {
            (&mut self.undo, &mut self.redo)
        } else {
            (&mut self.redo, &mut self.undo)
        };
        let Some((text, caret)) = from.pop() else {
            return;
        };
        to.push((std::mem::replace(&mut self.text, text), self.caret));
        self.caret = caret.min(self.text.len());
        self.run = None;
    }
}

/// How a grid of `body_lines` rows divides them: the pinned header takes one, the panel's
/// label one, the panel as many lines as the current cell wraps to (at least 1, at most 8 or a
/// third of the body), and the data rows the rest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GridRows {
    pub data: usize,
    pub panel: usize,
}

impl GridRows {
    /// The rows with the panel at its tallest: the fewest data rows any cell leaves, which a
    /// move keeps the cursor inside so a taller cell's panel never covers the cursor's row.
    #[must_use]
    pub fn of(body_lines: usize) -> Self {
        Self::fitted(body_lines, usize::MAX)
    }

    /// The rows with a panel of `wanted` lines, clamped to at least 1 and at most 8 or a third
    /// of the body.
    #[must_use]
    pub fn fitted(body_lines: usize, wanted: usize) -> Self {
        let body = body_lines.max(4);
        let panel = wanted.clamp(1, (body / 3).clamp(1, 8));
        Self {
            data: body.saturating_sub(panel.saturating_add(2)).max(1),
            panel,
        }
    }
}

/// One column the grid shows: its index, where it starts (cells from the grid's left edge, its
/// padding included), and its text width.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GridColumn {
    pub index: usize,
    pub x: usize,
    pub width: usize,
}

/// One cell as drawn: its clipped first line, and whether the record lacks it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GridCellView {
    pub text: String,
    pub missing: bool,
}

/// One record as drawn: a cell for each shown column.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GridRow {
    pub record: usize,
    pub cells: Vec<GridCellView>,
}

/// The current cell.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GridCursor {
    pub record: usize,
    pub column: usize,
    /// The 1-based text line the cell starts on.
    pub line: usize,
    pub quoted: bool,
    pub missing: bool,
    pub value: String,
    /// The header cell's value over this column, empty for a column past the header.
    pub header: String,
}

/// The panel under the grid: the current cell in full, or its open editor.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GridPanel {
    pub label: String,
    /// The wrapped lines shown.
    pub lines: Vec<String>,
    /// Each shown line's bytes in the value the panel shows (the open editor's text, else the
    /// cell's value), for [`GridProjection::panel_byte`].
    pub ranges: Vec<Range<usize>>,
    /// The first wrapped line shown, and how many there are.
    pub first_line: usize,
    pub total_lines: usize,
    /// The caret's shown line and cell column, while editing.
    pub caret: Option<(usize, usize)>,
    pub editing: bool,
}

/// A grid laid out for a face: what both faces draw.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GridProjection {
    pub delimiter: Delimiter,
    pub records: usize,
    pub columns: usize,
    pub rows_layout: GridRows,
    pub shown: Vec<GridColumn>,
    /// Whether `shown` starts with the first column pinned at the left edge, the scrolled
    /// columns after it.
    pub pinned: bool,
    pub hidden_left: bool,
    pub hidden_right: bool,
    pub header: GridRow,
    pub rows: Vec<GridRow>,
    pub cursor: GridCursor,
    pub panel: GridPanel,
    /// The 1-based line of the first problem the scan noted.
    pub problem_line: Option<usize>,
    /// The open cell editor, whole.
    pub draft: Option<CellDraft>,
}

impl EditorBufferView {
    /// The grid face's state, when the view shows a grid.
    #[inline]
    #[must_use]
    pub const fn grid(&self) -> Option<&GridState> {
        match &self.face {
            Face::Grid(grid) => Some(grid),
            Face::Text => None,
        }
    }

    /// The open cell editor, when there is one.
    #[inline]
    #[must_use]
    pub fn grid_draft(&self) -> Option<&CellDraft> {
        self.grid().and_then(|grid| grid.draft.as_ref())
    }

    /// Tell a grid how many character cells wide its face draws a row: its sideways scroll
    /// then stays where the face shows it. Whether that changed; nothing for a text view.
    pub fn set_grid_width(&mut self, cells: usize) -> bool {
        match self.grid_mut() {
            Some(grid) if grid.cells_wide != cells => {
                grid.cells_wide = cells;
                true
            }
            _ => false,
        }
    }

    #[inline]
    fn grid_mut(&mut self) -> Option<&mut GridState> {
        match &mut self.face {
            Face::Grid(grid) => Some(grid),
            Face::Text => None,
        }
    }
}

/// The current cell of `view` in `table`. An open cell editor's cell is its own record's
/// ([`CellDraft::record`]) at its column, wherever the caret went. Otherwise it is the caret's
/// record, and the remembered column while the caret is still in that cell (or at the record's
/// end for a cell it lacks), else the cell the caret is in.
#[must_use]
pub fn current_cell(table: &Table, view: &EditorBufferView) -> (usize, usize) {
    if let Some(draft) = view.grid_draft() {
        return (table.record_at(draft.record.start), draft.column);
    }
    let head = view.primary_selection().head;
    let record = table.record_at(head);
    let column = view.grid().map_or(0, |grid| grid.column);
    let Some(row) = table.records.get(record) else {
        return (0, 0);
    };
    let keeps = match row.cells.get(column) {
        Some(cell) => cell.holds(head),
        None => head == row.raw.end && column < table.columns().max(row.cells.len()).max(1),
    };
    (
        record,
        if keeps {
            column
        } else {
            table.cell_at(record, head)
        },
    )
}

/// The narrowest cap [`column_cap`] puts on a column, in character cells.
const MIN_COLUMN_CAP: usize = 12;

/// The widest a column is drawn in a grid `cells_wide` across (0 for not yet known): a quarter
/// of the row, from [`MIN_COLUMN_CAP`] to [`table::MAX_COLUMN_WIDTH`], so a long-text column
/// leaves room beside it for the columns that say which row it is.
fn column_cap(cells_wide: usize) -> usize {
    if cells_wide == 0 {
        return table::MAX_COLUMN_WIDTH;
    }
    (cells_wide / 4).clamp(MIN_COLUMN_CAP, table::MAX_COLUMN_WIDTH)
}

/// Every column's width for a grid `cells_wide` across whose cursor is in `column`: the
/// table's widths under [`column_cap`], and the narrowest width for a column only the cursor
/// reaches.
fn grid_widths(text: &str, table: &Table, column: usize, cells_wide: usize) -> Vec<usize> {
    let columns = table.columns().max(column.saturating_add(1)).max(1);
    let cap = column_cap(cells_wide);
    let mut widths: Vec<usize> = table
        .column_widths(text)
        .iter()
        .map(|&width| width.min(cap))
        .collect();
    widths.resize(columns, table::MIN_COLUMN_WIDTH);
    widths
}

/// The cells column 0 takes pinned at the left edge of a grid `cells_wide` across, padding
/// included, when it may be pinned there: the grid has other columns, and it takes at most a
/// third of the row.
fn pin_slot(widths: &[usize], cells_wide: usize) -> Option<usize> {
    let slot = widths.first()?.saturating_add(2);
    (widths.len() > 1 && slot.saturating_mul(3) <= cells_wide).then_some(slot)
}

/// How a grid shows its columns: the first scrolled column, and whether column 0 is pinned at
/// the left edge before it. Scrolled sideways, the grid shows from `left` as it stands;
/// otherwise from where [`effective_left`] keeps the cursor's column in view, in the room the
/// pinned column leaves once the grid has scrolled past column 0.
fn column_layout(
    widths: &[usize],
    left: usize,
    scrolled: bool,
    cursor: usize,
    cells_wide: usize,
) -> (usize, bool) {
    let pin = pin_slot(widths, cells_wide);
    if scrolled {
        let left = left.min(widths.len().saturating_sub(1));
        return (left, left > 0 && pin.is_some());
    }
    let unpinned = effective_left(widths, left, cursor, cells_wide);
    match pin {
        Some(slot) if unpinned > 0 => (
            effective_left(widths, left.max(1), cursor, cells_wide.saturating_sub(slot)).max(1),
            true,
        ),
        _ => (unpinned, false),
    }
}

/// The furthest a sideways scroll goes: the first column from which the rest fit, 0 when they
/// all do.
fn last_left(widths: &[usize], cells_wide: usize) -> usize {
    let slots = |range: &[usize]| {
        range.iter().fold(0usize, |acc, width| {
            acc.saturating_add(width.saturating_add(2))
        })
    };
    if slots(widths) <= cells_wide {
        return 0;
    }
    let room = cells_wide.saturating_sub(pin_slot(widths, cells_wide).unwrap_or(0));
    (1..widths.len())
        .find(|&from| widths.get(from..).is_some_and(|rest| slots(rest) <= room))
        .unwrap_or(widths.len().saturating_sub(1))
}

/// Where the grid's columns start so `cursor` shows: from `left` when the cursor fits there,
/// else from the cursor when it lies left of `left`, else from the first column that lets it fit.
fn effective_left(widths: &[usize], left: usize, cursor: usize, cells_wide: usize) -> usize {
    let fits = |from: usize| {
        widths
            .get(from..=cursor)
            .map(|span| {
                span.iter().fold(0usize, |acc, width| {
                    acc.saturating_add(width.saturating_add(2))
                })
            })
            .is_some_and(|used| used <= cells_wide)
    };
    if cursor < left {
        return cursor;
    }
    if fits(left) {
        return left;
    }
    (left..=cursor).find(|&from| fits(from)).unwrap_or(cursor)
}

/// Lay `table` (the scan of `text`) out as `view`'s grid in `body_lines` rows and `cells_wide`
/// character cells.
#[must_use]
pub fn project_grid(
    text: &str,
    table: &Table,
    view: &EditorBufferView,
    body_lines: usize,
    cells_wide: usize,
) -> GridProjection {
    let cells_wide = cells_wide.max(MIN_PANEL_WIDTH);
    let (record, column) = current_cell(table, view);
    let records = table.records.len();
    let cell = table.cell(record, column);
    let value = cell.map_or(String::new(), |cell| cell.value(text).into_owned());
    let draft = view.grid_draft();
    let shown_value = draft.map_or(value.as_str(), |draft| draft.text.as_str());
    let wrapped = table::wrap(shown_value, cells_wide);
    let layout = GridRows::fitted(body_lines, wrapped.len());
    let widths = grid_widths(text, table, column, cells_wide);
    let columns = widths.len();
    let (stored_left, scrolled) = view
        .grid()
        .map_or((0, false), |grid| (grid.left_column, grid.scrolled));
    let (left, pinned) = column_layout(&widths, stored_left, scrolled, column, cells_wide);
    let mut shown = Vec::new();
    let mut x = 0usize;
    if pinned && let Some(&width) = widths.first() {
        shown.push(GridColumn { index: 0, x, width });
        x = width.saturating_add(2);
    }
    let first_scrolled = shown.len();
    for (index, width) in widths.iter().enumerate().skip(left) {
        let slot = width.saturating_add(2);
        if x.saturating_add(slot) > cells_wide {
            if shown.len() == first_scrolled {
                shown.push(GridColumn {
                    index,
                    x,
                    width: cells_wide.saturating_sub(x).saturating_sub(2).max(1),
                });
            }
            break;
        }
        shown.push(GridColumn {
            index,
            x,
            width: *width,
        });
        x = x.saturating_add(slot);
    }
    let hidden_right = shown.last().is_some_and(|last| last.index + 1 < columns);
    let row_of = |record: usize| GridRow {
        record,
        cells: shown
            .iter()
            .map(|shown| match table.cell(record, shown.index) {
                Some(cell) => GridCellView {
                    text: table::clip(&cell.value(text), shown.width),
                    missing: false,
                },
                None => GridCellView {
                    text: String::new(),
                    missing: true,
                },
            })
            .collect(),
    };
    let max_top = records.saturating_sub(layout.data).max(1);
    let top = view
        .grid()
        .map_or(1, |grid| grid.top_record)
        .clamp(1, max_top);
    let mut rows: Vec<GridRow> = (top..records.min(top.saturating_add(layout.data)))
        .map(row_of)
        .collect();
    let header = table
        .cell(0, column)
        .map_or(String::new(), |cell| cell.value(text).into_owned());
    let cursor = GridCursor {
        record,
        column,
        line: table::line_of(text, table.caret_for(record, column)),
        quoted: cell.is_some_and(|cell| cell.quoted),
        missing: cell.is_none(),
        value: value.clone(),
        header: header.clone(),
    };
    let header_name = table::first_line(&header);
    let label = if header_name.trim().is_empty() || record == 0 {
        format!(
            "Row {} · Column {}",
            record.saturating_add(1),
            column.saturating_add(1)
        )
    } else {
        format!("Row {} · {}", record.saturating_add(1), header_name)
    };
    let caret = draft.map(|draft| table::wrapped_caret(&draft.text, &wrapped, draft.caret));
    let first_line = caret.map_or(0, |(line, _)| {
        line.saturating_add(1).saturating_sub(layout.panel)
    });
    let ranges: Vec<Range<usize>> = wrapped
        .iter()
        .skip(first_line)
        .take(layout.panel)
        .cloned()
        .collect();
    let lines = ranges
        .iter()
        .map(|range| {
            table::display(shown_value.get(range.clone()).unwrap_or_default()).into_owned()
        })
        .collect();
    let panel = GridPanel {
        label,
        lines,
        ranges,
        first_line,
        total_lines: wrapped.len(),
        caret: caret.map(|(line, column)| (line.saturating_sub(first_line), column)),
        editing: draft.is_some(),
    };
    let mut header_row = row_of(0);
    // The open cell shows what is being typed into it, as a spreadsheet's does.
    if let Some(draft) = draft {
        let at = shown.iter().position(|shown| shown.index == column);
        let row = if record == 0 {
            Some(&mut header_row)
        } else {
            rows.iter_mut().find(|row| row.record == record)
        };
        if let (Some(at), Some(row)) = (at, row)
            && let (Some(cell), Some(shown)) = (row.cells.get_mut(at), shown.get(at))
        {
            *cell = GridCellView {
                text: table::clip(&draft.text, shown.width),
                missing: false,
            };
        }
    }
    GridProjection {
        delimiter: table.delimiter,
        records,
        columns,
        rows_layout: layout,
        shown,
        pinned,
        hidden_left: left > usize::from(pinned),
        hidden_right,
        header: header_row,
        rows,
        cursor,
        panel,
        problem_line: table.first_problem_line(text),
        draft: draft.cloned(),
    }
}

/// What a grid's readers other than the person's eyes (the window's accessibility tree) get of
/// one cell.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum CellReading {
    /// Its whole value.
    Value(String),
    /// The record has no cell there.
    Missing,
    /// Concealed ([`Concealed`]): read out as [`HIDDEN_VALUE`], never its value.
    Hidden,
}

impl CellReading {
    /// What a reader is told of the cell: its value, `empty`, `no cell`, or [`HIDDEN_VALUE`].
    #[must_use]
    pub fn words(&self) -> &str {
        match self {
            Self::Value(value) if value.is_empty() => "empty",
            Self::Value(value) => value,
            Self::Missing => "no cell",
            Self::Hidden => HIDDEN_VALUE,
        }
    }
}

/// What a grid's readers other than the person's eyes get (one-mesh design §4.8): the table's
/// shape, the shown columns' headers and the shown records' cells, each whole (the face clips
/// them to its columns), and the current cell, every concealed one [`CellReading::Hidden`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GridReading {
    /// The data records (the header row is not one) and the columns.
    pub rows: usize,
    pub columns: usize,
    /// Each shown column: its index and its header (record 0).
    pub headers: Vec<(usize, CellReading)>,
    /// Each shown data record: its record number and a cell per shown column.
    pub records: Vec<(usize, Vec<CellReading>)>,
    /// The current cell: its record and column, its column's header, and the cell.
    pub cursor_at: (usize, usize),
    pub cursor_header: CellReading,
    pub cursor: CellReading,
}

impl GridReading {
    /// What a reader is told of the current cell: `Row 2, addr: ada@example.test`, the column
    /// named by its number where its header is the cell itself, empty or hidden.
    #[must_use]
    pub fn cursor_words(&self) -> String {
        let (record, column) = self.cursor_at;
        let header = match &self.cursor_header {
            CellReading::Value(header) if record > 0 => table::first_line(header).trim(),
            _ => "",
        };
        let column = if header.is_empty() {
            format!("column {}", column + 1)
        } else {
            header.to_string()
        };
        format!("Row {}, {column}: {}", record + 1, self.cursor.words())
    }
}

/// Whether the cell at `record`, `column` of `table` (the scan of `text`) is concealed by
/// `concealed`: every cell under [`Concealed::All`]; a data cell of a named column under
/// [`Concealed::Columns`], a header never.
#[must_use]
pub fn cell_concealed(
    concealed: Option<&Concealed>,
    text: &str,
    table: &Table,
    record: usize,
    column: usize,
) -> bool {
    let Some(concealed) = concealed else {
        return false;
    };
    if record == 0 {
        return concealed.headers();
    }
    let header = table
        .cell(0, column)
        .map_or(std::borrow::Cow::Borrowed(""), |cell| cell.value(text));
    concealed.column(&header)
}

/// What a grid's readers get of `projection`, the grid laid out from `table` (the scan of
/// `text`), under `concealed` ([`GridReading`]). The current cell reads its stored value, never
/// an open editor's text.
#[must_use]
pub fn read_grid(
    text: &str,
    table: &Table,
    projection: &GridProjection,
    concealed: Option<&Concealed>,
) -> GridReading {
    let read = |record: usize, column: usize| {
        if cell_concealed(concealed, text, table, record, column) {
            CellReading::Hidden
        } else {
            table
                .cell(record, column)
                .map_or(CellReading::Missing, |cell| {
                    CellReading::Value(cell.value(text).into_owned())
                })
        }
    };
    let shown = |record: usize| {
        projection
            .shown
            .iter()
            .map(|column| read(record, column.index))
            .collect::<Vec<_>>()
    };
    let cursor_at = (projection.cursor.record, projection.cursor.column);
    GridReading {
        rows: projection.records.saturating_sub(1),
        columns: projection.columns,
        headers: projection
            .shown
            .iter()
            .map(|column| column.index)
            .zip(shown(0))
            .collect(),
        records: projection
            .rows
            .iter()
            .map(|row| (row.record, shown(row.record)))
            .collect(),
        cursor_at,
        cursor_header: read(0, cursor_at.1),
        cursor: read(cursor_at.0, cursor_at.1),
    }
}

impl GridProjection {
    /// The value the panel shows: the open editor's text, else the current cell's value.
    #[must_use]
    pub fn panel_value(&self) -> &str {
        self.draft
            .as_ref()
            .map_or(self.cursor.value.as_str(), |draft| draft.text.as_str())
    }

    /// The byte of [`Self::panel_value`] at the boundary nearest a press in the panel's shown
    /// line `line` (0 for its first shown line), `half_cells` half character cells in (the
    /// press's offset in cells, doubled and rounded down, so a wide character's halves are told
    /// apart with one rounding): the boundary before the grapheme under it when the press is in
    /// its first half, else the one after, never inside a grapheme. `None` past the shown lines.
    #[must_use]
    pub fn panel_byte(&self, line: usize, half_cells: usize) -> Option<usize> {
        let range = self.panel.ranges.get(line)?.clone();
        let shown = self.panel_value().get(range.clone())?;
        let mut used = 0usize;
        for (at, grapheme) in shown.grapheme_indices() {
            let width = table::grapheme_cells(grapheme).saturating_mul(2);
            if half_cells < used.saturating_add(width) {
                let past_half = half_cells.saturating_sub(used).saturating_mul(2) >= width;
                let end = if past_half { grapheme.len() } else { 0 };
                return Some(range.start.saturating_add(at).saturating_add(end));
            }
            used = used.saturating_add(width);
        }
        Some(range.end)
    }
}

/// The narrowest panel the projection lays out, in cells.
const MIN_PANEL_WIDTH: usize = 8;

/// Where a grid move goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Up,
    Down,
    Left,
    Right,
    RowStart,
    RowEnd,
    FirstRow,
    LastRow,
    PageUp,
    PageDown,
    /// Right, wrapping to the next row's first cell (Tab).
    Next,
    /// Left, wrapping to the previous row's last cell (S-Tab).
    Previous,
}

/// How an open cell editor's commit moves on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Then {
    Stay,
    Move(Step),
}

impl EditorWorkspace {
    /// The scan of `snapshot` by `delimiter`, kept until the revision changes.
    fn grid_table(
        &mut self,
        snapshot: &DocumentSnapshot,
        delimiter: Delimiter,
    ) -> Result<Arc<Table>, TooLarge> {
        self.tables
            .table(snapshot.id, snapshot.seq, delimiter, &snapshot.text)
    }

    /// Show `view` as a grid split by `delimiter`, or as text for `None`.
    ///
    /// The grid collapses the view's selections to one caret at the start of the cell it was
    /// in. Nothing in the document changes.
    ///
    /// # Errors
    ///
    /// Why the face did not change: a file too large for the grid ([`TooLarge::reason`]), or a
    /// grid with an open cell editor ([`DRAFT_HOLDS`]).
    pub fn set_face(
        &mut self,
        store: &DocumentStore,
        view: &mut EditorBufferView,
        delimiter: Option<Delimiter>,
    ) -> Result<(), String> {
        let Some(delimiter) = delimiter else {
            if view.grid_draft().is_some() {
                return Err(DRAFT_HOLDS.to_string());
            }
            view.face = Face::Text;
            return Ok(());
        };
        let snapshot = store
            .snapshot(view.document)
            .ok_or_else(|| "the document is gone".to_string())?;
        let table = self
            .grid_table(&snapshot, delimiter)
            .map_err(TooLarge::reason)?;
        let head = view.primary_selection().head;
        let record = table.record_at(head);
        let column = table.cell_at(record, head);
        view.face = Face::Grid(GridState {
            delimiter,
            column,
            top_record: 1,
            left_column: 0,
            scrolled: false,
            cells_wide: 0,
            draft: None,
        });
        grid_place(view, &table, &snapshot.text, record, column);
        Ok(())
    }

    /// The delimiter `view`'s document's file type has, from its name and first line.
    fn document_delimiter(store: &DocumentStore, view: &EditorBufferView) -> Option<Delimiter> {
        let uri = store.canonical_uri(view.document)?;
        let snapshot = store.snapshot(view.document)?;
        Language::detect(uri, table::first_line(&snapshot.text)).delimiter()
    }

    /// `toggle-grid-view` (C-c C-t, ⌘E): flip between the text and the grid of a table.
    pub(super) fn toggle_grid(
        &mut self,
        store: &DocumentStore,
        view: &mut EditorBufferView,
    ) -> Vec<EditorEffect> {
        let target = if view.grid().is_some() {
            None
        } else {
            match Self::document_delimiter(store, view) {
                // The terminal face draws text alone (its grid is a later slice): a grid face
                // there would read keys as cells while showing lines.
                Some(_) if self.keymap.profile() == Profile::Terminal => {
                    return vec![EditorEffect::Status(TERMINAL_TABLE.to_string())];
                }
                Some(delimiter) => Some(delimiter),
                None => {
                    return vec![EditorEffect::Status(
                        "Not a table: only .csv and .tsv files have a grid view".to_string(),
                    )];
                }
            }
        };
        match self.set_face(store, view, target) {
            Ok(()) => Vec::new(),
            Err(refusal) => vec![EditorEffect::Status(refusal)],
        }
    }

    /// Scroll `view` by `delta` lines, or rows in the grid face, leaving the caret where it is.
    pub fn scroll_view(
        &mut self,
        store: &DocumentStore,
        view: &mut EditorBufferView,
        delta: i32,
    ) -> Result<(), EditorError> {
        let snapshot = store
            .snapshot(view.document)
            .ok_or(EditorError::UnknownDocument)?;
        let Some(delimiter) = view.grid().map(|grid| grid.delimiter) else {
            view.scroll_lines(&snapshot.text, delta);
            return Ok(());
        };
        let records = self
            .grid_table(&snapshot, delimiter)
            .map_or(1, |table| table.records.len());
        let data = GridRows::of(view.viewport_lines()).data;
        let max_top = records.saturating_sub(data).max(1);
        if let Some(grid) = view.grid_mut() {
            let top = grid.top_record.clamp(1, max_top);
            grid.top_record = if delta < 0 {
                top.saturating_sub(delta.unsigned_abs() as usize)
            } else {
                top.saturating_add(delta as usize)
            }
            .clamp(1, max_top);
        }
        Ok(())
    }

    /// Scroll a grid `delta` whole columns sideways (a sideways wheel or swipe, a press on a
    /// `◂`/`▸` mark), leaving the cursor where it is, perhaps off to the side, until it next
    /// moves. With the first column pinned, a step never stops where the pinned column and the
    /// scrolled ones show what the unscrolled grid showed. Nothing happens in a text view or with
    /// a cell open, whose cell stays in view.
    ///
    /// # Errors
    ///
    /// [`EditorError::UnknownDocument`] when the view's document is gone.
    pub fn scroll_grid_columns(
        &mut self,
        store: &DocumentStore,
        view: &mut EditorBufferView,
        delta: i32,
    ) -> Result<(), EditorError> {
        let Some(grid) = view.grid() else {
            return Ok(());
        };
        if grid.draft.is_some() || delta == 0 {
            return Ok(());
        }
        let (delimiter, column, cells_wide) = (
            grid.delimiter,
            grid.column,
            grid.cells_wide.max(MIN_PANEL_WIDTH),
        );
        let snapshot = snapshot_for_view(store, view)?;
        let Ok(table) = self.grid_table(&snapshot, delimiter) else {
            return Ok(());
        };
        let widths = grid_widths(&snapshot.text, &table, column, cells_wide);
        let last = last_left(&widths, cells_wide);
        let pinned = pin_slot(&widths, cells_wide).is_some();
        if let Some(grid) = view.grid_mut() {
            let (from, _) =
                column_layout(&widths, grid.left_column, grid.scrolled, column, cells_wide);
            let steps = delta.unsigned_abs() as usize;
            let mut left = if delta < 0 {
                from.saturating_sub(steps)
            } else {
                from.saturating_add(steps)
            }
            .min(last);
            // Pinned, column 1 first shows exactly what column 0 first does.
            if pinned && left == 1 {
                left = if delta < 0 { 0 } else { 2.min(last) };
            }
            grid.left_column = left;
            grid.scrolled = true;
        }
        Ok(())
    }

    /// Open the cell editor on the current cell for an input method's composition, as typing
    /// does: the composed text will replace the value. Nothing happens outside the grid face or
    /// with a cell already open.
    pub fn grid_compose(
        &mut self,
        store: &DocumentStore,
        view: &mut EditorBufferView,
    ) -> Result<(), EditorError> {
        if view.grid().is_none() || view.grid_draft().is_some() || view.minibuffer_active() {
            return Ok(());
        }
        self.open_draft(store, view, Some(String::new())).map(drop)
    }

    /// Commit an open cell editor before its view closes, as Return would without moving.
    ///
    /// # Errors
    ///
    /// Why the cell did not commit ([`CELL_CHANGED`], [`ROW_UNCLOSED`]), so the close waits and
    /// the person's text stays.
    pub fn commit_before_close(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
    ) -> Result<(), String> {
        if view.grid_draft().is_none() {
            return Ok(());
        }
        match self.commit_draft(store, view, Then::Stay) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(refusal)) => Err(refusal),
            Err(error) => Err(format!("{error:?}")),
        }
    }

    /// The grid face's reading of one input (the face-independent part of
    /// [`Self::handle_input`] for a grid view with no prompt open).
    pub(crate) fn grid_input(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
        input: EditorInput,
    ) -> Result<Reduction, EditorError> {
        match input {
            EditorInput::Focus(_) => Ok(Reduction::ignored()),
            EditorInput::Text(text) => {
                view.end_escape_prefix();
                if view.grid_draft().is_some() {
                    self.draft_insert(view, &text);
                    Ok(Reduction::reveal(Vec::new()))
                } else {
                    self.open_draft(store, view, Some(text))
                        .map(Reduction::reveal)
                }
            }
            EditorInput::Paste(text) => {
                view.end_escape_prefix();
                if let Some(draft) = view.grid_mut().and_then(|grid| grid.draft.as_mut()) {
                    // A paste is its own undo step, apart from the typing around it.
                    draft.end_run();
                    self.draft_insert(view, &text);
                    if let Some(draft) = view.grid_mut().and_then(|grid| grid.draft.as_mut()) {
                        draft.end_run();
                    }
                    return Ok(Reduction::reveal(Vec::new()));
                }
                let value = self.pasted_value(&text);
                self.set_current_cell(store, view, value)
                    .map(Reduction::reveal)
            }
            EditorInput::GridPanelPointer { byte } => {
                view.end_escape_prefix();
                if view.grid_draft().is_none() {
                    let effects = self.open_draft(store, view, None)?;
                    if view.grid_draft().is_none() {
                        return Ok(Reduction::reveal(effects));
                    }
                }
                if let Some(draft) = view.grid_mut().and_then(|grid| grid.draft.as_mut()) {
                    let mut caret = byte.min(draft.text.len());
                    while !draft.text.is_char_boundary(caret) {
                        caret = caret.saturating_sub(1);
                    }
                    draft.caret = caret;
                    draft.end_run();
                }
                Ok(Reduction::reveal(Vec::new()))
            }
            EditorInput::Pointer { byte, .. } => {
                if view.grid_draft().is_some() {
                    return Ok(Reduction::ignored());
                }
                let snapshot = snapshot_for_view(store, view)?;
                let Ok(table) = self.grid_table(&snapshot, grid_delimiter(view)) else {
                    return Ok(Reduction::ignored());
                };
                let record = table.record_at(byte);
                let column = table.cell_at(record, byte);
                grid_place(view, &table, &snapshot.text, record, column);
                Ok(Reduction::reveal(Vec::new()))
            }
            EditorInput::GridPointer {
                record,
                column,
                open,
            } => {
                if view.grid_draft().is_some() {
                    match self.commit_draft(store, view, Then::Stay)? {
                        Ok(()) => {}
                        Err(refusal) => {
                            return Ok(Reduction::reveal(vec![EditorEffect::Status(refusal)]));
                        }
                    }
                }
                let snapshot = snapshot_for_view(store, view)?;
                let Ok(table) = self.grid_table(&snapshot, grid_delimiter(view)) else {
                    return Ok(Reduction::ignored());
                };
                let record = record.min(table.records.len().saturating_sub(1));
                let column = column.min(table.columns().max(1).saturating_sub(1));
                grid_place(view, &table, &snapshot.text, record, column);
                if open {
                    return self.open_draft(store, view, None).map(Reduction::reveal);
                }
                Ok(Reduction::reveal(Vec::new()))
            }
            EditorInput::GridScroll { columns } => {
                self.scroll_grid_columns(store, view, columns)?;
                Ok(Reduction::reveal(Vec::new()))
            }
            EditorInput::Key { key, mods, text } => {
                let (key, text) = match key.main_block_twin() {
                    Some(Key::Character(glyph)) => (Key::Character(glyph), Some(glyph.to_string())),
                    Some(twin) => (twin, text),
                    None => (key, text),
                };
                if view.grid_draft().is_some() {
                    self.draft_key(store, view, key, mods, text)
                } else {
                    self.grid_key(store, view, key, mods, text)
                }
            }
        }
    }

    /// A key in the grid with no cell open.
    fn grid_key(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
        key: Key,
        mods: Modifiers,
        text: Option<String>,
    ) -> Result<Reduction, EditorError> {
        let profile = self.keymap.profile();
        if mods.contains(Modifiers::SUPER) {
            if !profile.super_shortcuts() || mods.contains(Modifiers::HYPER) {
                return Ok(Reduction::ignored());
            }
            let command = match key {
                Key::Character('s' | 'S') => EditorCommand::Save,
                Key::Character('z' | 'Z') if mods.contains(Modifiers::SHIFT) => EditorCommand::Redo,
                Key::Character('z' | 'Z') => EditorCommand::Undo,
                Key::Character('y' | 'Y') => EditorCommand::Redo,
                Key::Character('e' | 'E') => EditorCommand::ToggleGridView,
                Key::Character('c' | 'C') => EditorCommand::KillRingSave,
                Key::Character('x' | 'X') => EditorCommand::KillRegion,
                _ => return Ok(Reduction::ignored()),
            };
            return self.execute(store, view, command).map(Reduction::reveal);
        }
        if mods.contains(Modifiers::HYPER) {
            return Ok(Reduction::ignored());
        }
        if matches!(key, Key::Named(NamedKey::Tab))
            && !view.chord_pending()
            && !mods.intersects(Modifiers::CTRL | Modifiers::ALT | Modifiers::META)
        {
            let step = if mods.contains(Modifiers::SHIFT) {
                Step::Previous
            } else {
                Step::Next
            };
            return self.grid_step(store, view, step, 1).map(Reduction::reveal);
        }
        let escape_meta = profile.escape_is_meta();
        if view.chord_pending()
            || mods.intersects(Modifiers::CTRL | Modifiers::ALT | Modifiers::META)
            || (escape_meta && matches!(key, Key::Named(NamedKey::Escape)))
        {
            return match KeyChord::from_key(&key, mods) {
                Some(chord) => self.handle_chord(store, view, chord).map(Reduction::reveal),
                None => Ok(Reduction::ignored()),
            };
        }
        let command = match key {
            Key::Character(character) => {
                let typed = text.unwrap_or_else(|| character.to_string());
                if view.accept_argument_digit(&typed) {
                    return Ok(Reduction::reveal(Vec::new()));
                }
                return self
                    .open_draft(store, view, Some(typed))
                    .map(Reduction::reveal);
            }
            Key::Named(NamedKey::Space) => {
                return self
                    .open_draft(store, view, Some(" ".to_string()))
                    .map(Reduction::reveal);
            }
            Key::Named(NamedKey::Enter) if mods.contains(Modifiers::SHIFT) => {
                EditorCommand::MoveLineUp
            }
            Key::Named(NamedKey::Enter | NamedKey::F2) => {
                return self.open_draft(store, view, None).map(Reduction::reveal);
            }
            Key::Named(NamedKey::Backspace) => EditorCommand::DeleteBackward,
            Key::Named(NamedKey::Delete) => EditorCommand::DeleteForward,
            Key::Named(NamedKey::ArrowLeft) => EditorCommand::MoveBackward,
            Key::Named(NamedKey::ArrowRight) => EditorCommand::MoveForward,
            Key::Named(NamedKey::ArrowUp) => EditorCommand::MoveLineUp,
            Key::Named(NamedKey::ArrowDown) => EditorCommand::MoveLineDown,
            Key::Named(NamedKey::Home) => EditorCommand::MoveLineStart,
            Key::Named(NamedKey::End) => EditorCommand::MoveLineEnd,
            Key::Named(NamedKey::PageUp) => EditorCommand::ScrollPageUp,
            Key::Named(NamedKey::PageDown) => EditorCommand::ScrollPageDown,
            Key::Named(NamedKey::Escape) => EditorCommand::Abort,
            _ => return Ok(Reduction::ignored()),
        };
        self.execute(store, view, command).map(Reduction::reveal)
    }

    /// A key while the cell editor is open: it edits the draft, or commits or drops it.
    fn draft_key(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
        key: Key,
        mods: Modifiers,
        text: Option<String>,
    ) -> Result<Reduction, EditorError> {
        let status = |words: String| Ok(Reduction::reveal(vec![EditorEffect::Status(words)]));
        if mods.contains(Modifiers::SUPER) {
            if !self.keymap.profile().super_shortcuts() {
                return Ok(Reduction::ignored());
            }
            return match key {
                Key::Character('s' | 'S') => self
                    .commit_then(store, view, EditorCommand::Save)
                    .map(Reduction::reveal),
                Key::Character('z' | 'Z') if mods.contains(Modifiers::SHIFT) => {
                    draft_history(view, false);
                    Ok(Reduction::reveal(Vec::new()))
                }
                Key::Character('z' | 'Z') => {
                    draft_history(view, true);
                    Ok(Reduction::reveal(Vec::new()))
                }
                Key::Character('y' | 'Y') => {
                    draft_history(view, false);
                    Ok(Reduction::reveal(Vec::new()))
                }
                Key::Character('e' | 'E') => status(DRAFT_HOLDS.to_string()),
                _ => Ok(Reduction::ignored()),
            };
        }
        let control = mods.contains(Modifiers::CTRL);
        let meta = mods.intersects(Modifiers::ALT | Modifiers::META);
        let shift = mods.contains(Modifiers::SHIFT);
        if let Some(first) = view.chord_prefix.first().cloned() {
            view.chord_prefix.clear();
            view.prefix_hud = None;
            let Some(second) = KeyChord::from_key(&key, mods) else {
                return Ok(Reduction::ignored());
            };
            return match (first.display().as_str(), second.display().as_str()) {
                ("C-x", "C-s") => self
                    .commit_then(store, view, EditorCommand::Save)
                    .map(Reduction::reveal),
                ("C-x", "u") => {
                    draft_history(view, true);
                    Ok(Reduction::reveal(Vec::new()))
                }
                ("C-c", "C-c") => self
                    .commit_then(store, view, EditorCommand::FinishEdit)
                    .map(Reduction::reveal),
                ("C-c", "C-k") => {
                    draft_drop(view);
                    self.execute(store, view, EditorCommand::AbortEdit)
                        .map(Reduction::reveal)
                }
                (_, "C-g") => status("Quit".to_string()),
                (first, second) => {
                    status(format!("{first} {second} is not a cell key: {DRAFT_HOLDS}"))
                }
            };
        }
        if control || meta {
            let Some(chord) = KeyChord::from_key(&key, mods) else {
                return Ok(Reduction::ignored());
            };
            match chord.display().as_str() {
                "C-g" => {
                    draft_drop(view);
                    return status("Quit".to_string());
                }
                "C-x" | "C-c" => {
                    view.prefix_hud = Some(format!(
                        "{} — {}",
                        chord.display(),
                        if chord.key == "x" {
                            "C-s saves, u restores the cell"
                        } else {
                            "C-c finishes, C-k aborts"
                        }
                    ));
                    view.chord_prefix.push(chord);
                    return Ok(Reduction::reveal(Vec::new()));
                }
                "C-b" => draft_motion(view, Motion::Back),
                "C-f" => draft_motion(view, Motion::Forward),
                "M-b" | "M-left" => draft_motion(view, Motion::WordBack),
                "M-f" | "M-right" => draft_motion(view, Motion::WordForward),
                "C-p" => draft_motion(view, Motion::Up),
                "C-n" => draft_motion(view, Motion::Down),
                "C-a" => draft_motion(view, Motion::LineStart),
                "C-e" => draft_motion(view, Motion::LineEnd),
                "C-d" => draft_delete(view, Motion::Forward),
                "M-d" => draft_delete(view, Motion::WordForward),
                "M-backspace" => draft_delete(view, Motion::WordBack),
                "C-k" => draft_kill_line(view),
                "C-/" | "C-_" => draft_history(view, true),
                "C-?" | "M-_" => draft_history(view, false),
                "M-enter" => self.draft_insert(view, "\n"),
                other => {
                    return status(format!("{other} is not a cell key: {DRAFT_HOLDS}"));
                }
            }
            return Ok(Reduction::reveal(Vec::new()));
        }
        match key {
            Key::Named(NamedKey::Enter) => {
                let step = if shift { Step::Up } else { Step::Down };
                return self
                    .commit_moving(store, view, Then::Move(step))
                    .map(Reduction::reveal);
            }
            Key::Named(NamedKey::Tab) => {
                let step = if shift { Step::Previous } else { Step::Next };
                return self
                    .commit_moving(store, view, Then::Move(step))
                    .map(Reduction::reveal);
            }
            Key::Named(NamedKey::Escape) => {
                draft_drop(view);
            }
            Key::Named(NamedKey::ArrowLeft) => draft_motion(view, Motion::Back),
            Key::Named(NamedKey::ArrowRight) => draft_motion(view, Motion::Forward),
            Key::Named(NamedKey::ArrowUp) => draft_motion(view, Motion::Up),
            Key::Named(NamedKey::ArrowDown) => draft_motion(view, Motion::Down),
            Key::Named(NamedKey::Home) => draft_motion(view, Motion::LineStart),
            Key::Named(NamedKey::End) => draft_motion(view, Motion::LineEnd),
            Key::Named(NamedKey::Backspace) => draft_delete(view, Motion::Back),
            Key::Named(NamedKey::Delete) => draft_delete(view, Motion::Forward),
            Key::Named(NamedKey::Space) => self.draft_insert(view, " "),
            Key::Character(character) => {
                let typed = text.unwrap_or_else(|| character.to_string());
                self.draft_insert(view, &typed);
            }
            _ => return Ok(Reduction::ignored()),
        }
        Ok(Reduction::reveal(Vec::new()))
    }

    /// The grid's reading of a command (the early arm of [`Self::execute_with`] for a grid view).
    pub(super) fn grid_run(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
        command: EditorCommand,
        argument: Option<PrefixArgument>,
        previous: Option<EditorCommand>,
        args: Vec<String>,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        let count = argument.map_or(1, PrefixArgument::count);
        if view.grid_draft().is_some() {
            return match command {
                EditorCommand::Abort => {
                    draft_drop(view);
                    view.cancel_transient();
                    Ok(vec![EditorEffect::Status("Quit".into())])
                }
                EditorCommand::Save | EditorCommand::FinishEdit => {
                    self.commit_then(store, view, command)
                }
                EditorCommand::KillBuffer | EditorCommand::SaveBuffersKillTerminal => {
                    match self.commit_draft(store, view, Then::Stay)? {
                        Ok(()) => self.run(store, view, command, argument, previous, args),
                        Err(refusal) => Ok(vec![EditorEffect::Status(refusal)]),
                    }
                }
                EditorCommand::AbortEdit => {
                    draft_drop(view);
                    self.run(store, view, command, argument, previous, args)
                }
                EditorCommand::Undo | EditorCommand::Redo => {
                    draft_history(view, command == EditorCommand::Undo);
                    Ok(Vec::new())
                }
                _ => Ok(vec![EditorEffect::Status(DRAFT_HOLDS.to_string())]),
            };
        }
        if !command.allowed_in_grid() {
            return Ok(vec![EditorEffect::Status(format!(
                "{} is not in the grid view: C-c C-t or Cmd-E shows the text",
                command.name()
            ))]);
        }
        let step = match command {
            EditorCommand::MoveBackward | EditorCommand::MoveWordBackward => Some(Step::Left),
            EditorCommand::MoveForward | EditorCommand::MoveWordForward => Some(Step::Right),
            EditorCommand::MoveLineUp => Some(Step::Up),
            EditorCommand::MoveLineDown => Some(Step::Down),
            EditorCommand::MoveLineStart => Some(Step::RowStart),
            EditorCommand::MoveLineEnd => Some(Step::RowEnd),
            EditorCommand::BeginningOfBuffer => Some(Step::FirstRow),
            EditorCommand::EndOfBuffer => Some(Step::LastRow),
            EditorCommand::ScrollPageUp => Some(Step::PageUp),
            EditorCommand::ScrollPageDown => Some(Step::PageDown),
            _ => None,
        };
        if let Some(step) = step {
            return self.grid_step(store, view, step, count);
        }
        match command {
            EditorCommand::DeleteBackward | EditorCommand::DeleteForward => {
                self.set_current_cell(store, view, "")
            }
            EditorCommand::KillRingSave => self.grid_copy(store, view),
            EditorCommand::KillRegion | EditorCommand::KillLine => self.grid_cut(store, view),
            EditorCommand::Yank => {
                let kill = self
                    .kills()
                    .0
                    .front()
                    .map(|kill| self.held_kill(kill))
                    .ok_or(EditorError::NoKill)?;
                let value = self.pasted_value(&kill);
                self.set_current_cell(store, view, value)
            }
            EditorCommand::OpenLine | EditorCommand::GridInsertRow => {
                self.grid_row(store, view, RowEdit::Insert)
            }
            EditorCommand::GridDeleteRow => self.grid_row(store, view, RowEdit::Delete),
            EditorCommand::Undo | EditorCommand::Redo | EditorCommand::UndoAgentEdit => {
                self.grid_history(store, view, command)
            }
            EditorCommand::RecenterTopBottom => {
                let snapshot = snapshot_for_view(store, view)?;
                if let Ok(table) = self.grid_table(&snapshot, grid_delimiter(view)) {
                    let (record, _) = current_cell(&table, view);
                    let data = GridRows::of(view.viewport_lines()).data;
                    let max_top = table.records.len().saturating_sub(data).max(1);
                    if let Some(grid) = view.grid_mut() {
                        grid.top_record = record.saturating_sub(data / 2).clamp(1, max_top);
                    }
                }
                Ok(Vec::new())
            }
            EditorCommand::ToggleGridView => Ok(self.toggle_grid(store, view)),
            _ => self.run(store, view, command, argument, previous, args),
        }
    }

    /// Move the grid cursor `count` steps.
    fn grid_step(
        &mut self,
        store: &DocumentStore,
        view: &mut EditorBufferView,
        step: Step,
        count: usize,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        let snapshot = snapshot_for_view(store, view)?;
        let table = match self.grid_table(&snapshot, grid_delimiter(view)) {
            Ok(table) => table,
            Err(too_large) => return Ok(vec![EditorEffect::Status(too_large.reason())]),
        };
        let (mut record, mut column) = current_cell(&table, view);
        let last_record = table.records.len().saturating_sub(1);
        let last_column = table.columns().max(1).saturating_sub(1);
        let page = GridRows::of(view.viewport_lines())
            .data
            .saturating_sub(1)
            .max(1);
        for _ in 0..count.max(1) {
            match step {
                Step::Up => record = record.saturating_sub(1),
                Step::Down => record = record.saturating_add(1).min(last_record),
                Step::Left => column = column.saturating_sub(1),
                Step::Right => column = column.saturating_add(1).min(last_column),
                Step::RowStart => column = 0,
                Step::RowEnd => {
                    let header = table.records.first().map_or(0, |row| row.cells.len());
                    let own = table.records.get(record).map_or(0, |row| row.cells.len());
                    column = own.max(header).saturating_sub(1);
                }
                Step::FirstRow => record = 0,
                Step::LastRow => record = last_record,
                Step::PageUp => record = record.saturating_sub(page),
                Step::PageDown => record = record.saturating_add(page).min(last_record),
                Step::Next if column < last_column => column += 1,
                Step::Next if record < last_record => {
                    record += 1;
                    column = 0;
                }
                Step::Previous if column > 0 => column -= 1,
                Step::Previous if record > 0 => {
                    record -= 1;
                    column = last_column;
                }
                Step::Next | Step::Previous => {}
            }
        }
        if matches!(step, Step::PageUp | Step::PageDown)
            && let Some(grid) = view.grid_mut()
        {
            let delta = page.saturating_mul(count.max(1));
            grid.top_record = if step == Step::PageUp {
                grid.top_record.saturating_sub(delta)
            } else {
                grid.top_record.saturating_add(delta)
            };
        }
        grid_place(view, &table, &snapshot.text, record, column);
        self.last_yank = None;
        Ok(Vec::new())
    }

    /// Undo or redo the document's last change, or undo its newest agent edit, then put the
    /// cursor on the cell it touched. The history's own words (a frame that was not yours) come
    /// back as its effects.
    fn grid_history(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
        command: EditorCommand,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        let effects = match command {
            EditorCommand::Undo => self.undo(store, view)?,
            EditorCommand::Redo => self.redo(store, view)?,
            _ => self.undo_agent_edit(store, view)?,
        };
        let touched = self
            .pending_commits
            .last()
            .and_then(|(_, _, deltas)| deltas.first())
            .map(|delta| (delta.old.start, delta.inserted_len));
        let snapshot = snapshot_for_view(store, view)?;
        if let (Some((at, inserted)), Ok(table)) =
            (touched, self.grid_table(&snapshot, grid_delimiter(view)))
        {
            let mut record = table.record_at(at);
            let mut column = table.cell_at(record, at);
            // A row put back after a last row with no line break comes in behind that row's new
            // break: the row changed is the one after the break, not the one before it.
            let brings_break = snapshot
                .text
                .get(at..at.saturating_add(inserted))
                .is_some_and(|text| text.starts_with(['\r', '\n']));
            if brings_break
                && table
                    .records
                    .get(record)
                    .is_some_and(|row| row.terminator.start == at)
                && record.saturating_add(1) < table.records.len()
            {
                record = record.saturating_add(1);
                column = 0;
            }
            grid_place(view, &table, &snapshot.text, record, column);
        }
        Ok(effects)
    }

    /// Open the cell editor on the current cell, holding `initial` (typing replaces the value)
    /// or the cell's own value, with the caret at its end, tied to the cell's record. A cell
    /// whose quote never closes is not opened ([`CELL_UNCLOSED`]): it holds every later row.
    fn open_draft(
        &mut self,
        store: &DocumentStore,
        view: &mut EditorBufferView,
        initial: Option<String>,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        if store.read_only(view.document) {
            return Ok(vec![EditorEffect::Status(
                super::workspace::read_only_words(store, view.document),
            )]);
        }
        let snapshot = snapshot_for_view(store, view)?;
        let Ok(table) = self.grid_table(&snapshot, grid_delimiter(view)) else {
            return Ok(Vec::new());
        };
        let (record, column) = current_cell(&table, view);
        if table.cell_unclosed(record, column) {
            return Ok(vec![EditorEffect::Status(CELL_UNCLOSED.to_string())]);
        }
        let original = table.cell(record, column).map_or(String::new(), |cell| {
            cell.value(&snapshot.text).into_owned()
        });
        let span = table
            .records
            .get(record)
            .map_or(0..0, |row| row.raw.clone());
        // Typing that replaces the value is the draft's first edit: undo gives the value back.
        let (undo, run) = if initial.is_some() {
            (
                vec![(original.clone(), original.len())],
                Some(DraftRun::Insert),
            )
        } else {
            (Vec::new(), None)
        };
        let text = initial.unwrap_or_else(|| original.clone());
        let caret = text.len();
        grid_place(view, &table, &snapshot.text, record, column);
        view.chord_prefix.clear();
        view.prefix_hud = None;
        view.prefix_argument = None;
        if let Some(grid) = view.grid_mut() {
            grid.draft = Some(CellDraft {
                column,
                record: span,
                moved: false,
                original,
                text,
                caret,
                undo,
                redo: Vec::new(),
                run,
            });
        }
        Ok(Vec::new())
    }

    /// Insert `text` at the draft's caret.
    fn draft_insert(&mut self, view: &mut EditorBufferView, text: &str) {
        if let Some(draft) = view.grid_mut().and_then(|grid| grid.draft.as_mut()) {
            draft.begin(DraftRun::Insert);
            let caret = draft.caret.min(draft.text.len());
            draft.text.insert_str(caret, text);
            draft.caret = caret.saturating_add(text.len());
        }
    }

    /// Commit the draft, then run `command` (save or finish) as its key would.
    fn commit_then(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
        command: EditorCommand,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        match self.commit_draft(store, view, Then::Stay)? {
            Ok(()) => self.execute(store, view, command),
            Err(refusal) => Ok(vec![EditorEffect::Status(refusal)]),
        }
    }

    /// Commit the draft and move as `then` says.
    fn commit_moving(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
        then: Then,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        match self.commit_draft(store, view, then)? {
            Ok(()) => Ok(Vec::new()),
            Err(refusal) => Ok(vec![EditorEffect::Status(refusal)]),
        }
    }

    /// Write the draft into its cell as one transaction over the cell's own bytes (none when
    /// the value is unchanged), close the editor, and move as `then` says. The cell is the draft's
    /// own ([`CellDraft::record`]), never wherever the caret went, and the commit lands only while
    /// that cell still holds the value the draft opened with ([`CELL_CHANGED`]). When another
    /// writer touched the record or met it at either end ([`CellDraft::moved`]), which cell is
    /// the draft's own is a guess: the commit refuses once, naming the cell it would write, ties
    /// the draft to that cell, and the next commit writes it.
    fn commit_draft(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
        then: Then,
    ) -> Result<Result<(), String>, EditorError> {
        let Some(draft) = view.grid_draft().cloned() else {
            return Ok(Ok(()));
        };
        let snapshot = snapshot_for_view(store, view)?;
        let table = match self.grid_table(&snapshot, grid_delimiter(view)) {
            Ok(table) => table,
            Err(too_large) => return Ok(Err(too_large.reason())),
        };
        let (record, column) = current_cell(&table, view);
        let current = table.cell(record, column).map_or(String::new(), |cell| {
            cell.value(&snapshot.text).into_owned()
        });
        if current != draft.original {
            return Ok(Err(CELL_CHANGED.to_string()));
        }
        let span = table
            .records
            .get(record)
            .map_or(0..0, |row| row.raw.clone());
        if draft.moved || span != draft.record {
            grid_place(view, &table, &snapshot.text, record, column);
            if let Some(draft) = view.grid_mut().and_then(|grid| grid.draft.as_mut()) {
                draft.record = span;
                draft.moved = false;
            }
            return Ok(Err(cell_moved(&table, &snapshot.text, record, column)));
        }
        match table::edit_cell(&table, &snapshot.text, record, column, &draft.text) {
            Ok(Some(edit)) => {
                self.apply_recorded(store, view, vec![edit])?;
                self.last_yank = None;
            }
            Ok(None) => {}
            Err(why) => return Ok(Err(unwritable(why).to_string())),
        }
        draft_drop(view);
        let snapshot = snapshot_for_view(store, view)?;
        if let Ok(table) = self.grid_table(&snapshot, grid_delimiter(view)) {
            grid_place(view, &table, &snapshot.text, record, column);
        }
        if let Then::Move(step) = then {
            self.grid_step(store, view, step, 1)?;
        }
        Ok(Ok(()))
    }

    /// `kill-ring-save` (M-w, ⌘C): the current cell's value, never its quoted bytes, onto the
    /// kill ring (and so the clipboard), the document untouched.
    fn grid_copy(
        &mut self,
        store: &DocumentStore,
        view: &mut EditorBufferView,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        let snapshot = snapshot_for_view(store, view)?;
        let table = match self.grid_table(&snapshot, grid_delimiter(view)) {
            Ok(table) => table,
            Err(too_large) => return Ok(vec![EditorEffect::Status(too_large.reason())]),
        };
        let (record, column) = current_cell(&table, view);
        let value = table.cell(record, column).map_or(String::new(), |cell| {
            cell.value(&snapshot.text).into_owned()
        });
        if value.is_empty() {
            return Ok(vec![EditorEffect::Status(EMPTY_CELL.to_string())]);
        }
        // A status is read out to the window's readers: a concealed value is never quoted there.
        let said = if cell_concealed(
            store.concealed(view.document),
            &snapshot.text,
            &table,
            record,
            column,
        ) {
            COPIED_HIDDEN.to_string()
        } else {
            format!("Copied “{}”", table::clip(&value, 32))
        };
        // A cell is a whole value: a copy never joins the kill before it.
        self.record_kill(value.clone(), None, false);
        *self.kills_mut().1 = Some(value);
        self.last_yank = None;
        Ok(vec![EditorEffect::Status(said)])
    }

    /// `kill-region` and `kill-line` (C-w, C-k, ⌘X): the current cell's value onto the kill ring,
    /// then the cell cleared, as one undo step.
    fn grid_cut(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        if store.read_only(view.document) {
            return Ok(vec![EditorEffect::Status(
                super::workspace::read_only_words(store, view.document),
            )]);
        }
        let snapshot = snapshot_for_view(store, view)?;
        let table = match self.grid_table(&snapshot, grid_delimiter(view)) {
            Ok(table) => table,
            Err(too_large) => return Ok(vec![EditorEffect::Status(too_large.reason())]),
        };
        let (record, column) = current_cell(&table, view);
        let edit = match table::edit_cell(&table, &snapshot.text, record, column, "") {
            Ok(Some(edit)) => edit,
            Ok(None) => return Ok(vec![EditorEffect::Status(EMPTY_CELL.to_string())]),
            Err(why) => return Ok(vec![EditorEffect::Status(unwritable(why).to_string())]),
        };
        let value = table.cell(record, column).map_or(String::new(), |cell| {
            cell.value(&snapshot.text).into_owned()
        });
        self.apply_recorded(store, view, vec![edit])?;
        // The kill lands once the clear has committed, as a text kill's does.
        self.record_kill(value.clone(), None, false);
        *self.kills_mut().1 = Some(value);
        self.last_yank = None;
        let snapshot = snapshot_for_view(store, view)?;
        if let Ok(table) = self.grid_table(&snapshot, grid_delimiter(view)) {
            grid_place(view, &table, &snapshot.text, record, column);
        }
        Ok(Vec::new())
    }

    /// Add an empty row below the current one and move to its first cell (`C-o`,
    /// `grid-insert-row`), or delete the current row (`C-S-backspace`, `grid-delete-row`), as
    /// one undo step over that row's own bytes ([`table::insert_row`], [`table::delete_row`]).
    fn grid_row(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
        row: RowEdit,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        let snapshot = snapshot_for_view(store, view)?;
        let table = match self.grid_table(&snapshot, grid_delimiter(view)) {
            Ok(table) => table,
            Err(too_large) => return Ok(vec![EditorEffect::Status(too_large.reason())]),
        };
        let (record, column) = current_cell(&table, view);
        let edit = match row {
            RowEdit::Insert => table::insert_row(&table, &snapshot.text, record),
            RowEdit::Delete => table::delete_row(&table, &snapshot.text, record),
        };
        let edit = match edit {
            Ok(Some(edit)) => edit,
            Ok(None) => return Ok(Vec::new()),
            Err(Unwritable::JoinsLineBreaks) => {
                return Ok(vec![EditorEffect::Status(ROW_JOINS_BREAKS.to_string())]);
            }
            Err(_) => return Ok(vec![EditorEffect::Status(ROW_HOLDS_REST.to_string())]),
        };
        self.apply_recorded(store, view, vec![edit])?;
        self.last_yank = None;
        let snapshot = snapshot_for_view(store, view)?;
        if let Ok(table) = self.grid_table(&snapshot, grid_delimiter(view)) {
            let (record, column) = match row {
                RowEdit::Insert => (record.saturating_add(1), 0),
                RowEdit::Delete => (record.min(table.records.len().saturating_sub(1)), column),
            };
            grid_place(view, &table, &snapshot.text, record, column);
        }
        Ok(Vec::new())
    }

    /// The value a paste or a yank of `text` puts in a cell: all of it when it is the value the
    /// grid's last copy or cut took (a cell's whole value, final line break and all), else
    /// [`cell_text`].
    fn pasted_value<'a>(&self, text: &'a str) -> &'a str {
        if self
            .kills()
            .1
            .is_some_and(|kill| self.held_kill(kill) == text)
        {
            text
        } else {
            cell_text(text)
        }
    }

    /// Give the current cell `value` as one undo step (DEL clears it, a paste replaces it).
    fn set_current_cell(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
        value: &str,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        if store.read_only(view.document) {
            return Ok(vec![EditorEffect::Status(
                super::workspace::read_only_words(store, view.document),
            )]);
        }
        let snapshot = snapshot_for_view(store, view)?;
        let table = match self.grid_table(&snapshot, grid_delimiter(view)) {
            Ok(table) => table,
            Err(too_large) => return Ok(vec![EditorEffect::Status(too_large.reason())]),
        };
        let (record, column) = current_cell(&table, view);
        let edit = match table::edit_cell(&table, &snapshot.text, record, column, value) {
            Ok(Some(edit)) => edit,
            Ok(None) => return Ok(Vec::new()),
            Err(why) => return Ok(vec![EditorEffect::Status(unwritable(why).to_string())]),
        };
        self.apply_recorded(store, view, vec![edit])?;
        self.last_yank = None;
        let snapshot = snapshot_for_view(store, view)?;
        if let Ok(table) = self.grid_table(&snapshot, grid_delimiter(view)) {
            grid_place(view, &table, &snapshot.text, record, column);
        }
        Ok(Vec::new())
    }

    /// Scroll a grid view to show the cell its primary caret is in, and set its column to that
    /// cell's, leaving the selection itself untouched (so an incremental search keeps its match
    /// range).
    pub(super) fn grid_reveal_caret(&mut self, store: &DocumentStore, view: &mut EditorBufferView) {
        if view.grid().is_none() || view.grid_draft().is_some() {
            return;
        }
        let Ok(snapshot) = snapshot_for_view(store, view) else {
            return;
        };
        let Ok(table) = self.grid_table(&snapshot, grid_delimiter(view)) else {
            return;
        };
        let head = view.primary_selection().head;
        let record = table.record_at(head);
        let column = table.cell_at(record, head);
        grid_scroll_to(view, &table, &snapshot.text, record, column);
    }
}

/// What a commit says when another writer changed the rows around an open cell: the cell the
/// next commit writes, by its row's first cell, so the person can check it is the one they meant.
fn cell_moved(table: &Table, text: &str, record: usize, column: usize) -> String {
    let who = table
        .cell(record, 0)
        .map(|cell| table::clip(&cell.value(text), 24))
        .filter(|who| !who.trim().is_empty() && column > 0)
        .map_or(String::new(), |who| format!(" ({who})"));
    let header = table
        .cell(0, column)
        .map(|cell| table::clip(&cell.value(text), 32))
        .filter(|header| !header.trim().is_empty() && record > 0)
        .unwrap_or_else(|| format!("column {}", column.saturating_add(1)));
    format!(
        "The file changed around this cell: Return again puts your text in row {}{who} · {header}; Escape drops it",
        record.saturating_add(1)
    )
}

/// Which row operation [`EditorWorkspace::grid_row`] makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowEdit {
    Insert,
    Delete,
}

/// The value a paste or a yank of text from outside the grid puts in a cell: the text with one
/// final line break dropped, as a copied row or line ends with one the cell never meant.
fn cell_text(text: &str) -> &str {
    text.strip_suffix("\r\n")
        .or_else(|| text.strip_suffix('\n'))
        .unwrap_or(text)
}

/// The delimiter of a grid view (a comma for a text view, which never asks).
fn grid_delimiter(view: &EditorBufferView) -> Delimiter {
    view.grid().map_or(Delimiter::Comma, |grid| grid.delimiter)
}

/// Put the grid cursor on `record`, `column`: one caret where [`Table::caret_for`] puts it, the
/// column remembered, and the rows scrolled to show it.
fn grid_place(
    view: &mut EditorBufferView,
    table: &Table,
    text: &str,
    record: usize,
    column: usize,
) {
    let caret = table.caret_for(record, column);
    view.selections = vec![Selection::caret(caret)];
    view.primary = 0;
    view.mark_active = false;
    view.desired_column = None;
    grid_scroll_to(view, table, text, record, column);
}

/// Remember `column` as the grid cursor's column and scroll the grid's rows and columns to show
/// `record`, `column`, leaving the view's selections as they stand.
fn grid_scroll_to(
    view: &mut EditorBufferView,
    table: &Table,
    text: &str,
    record: usize,
    column: usize,
) {
    let data = GridRows::of(view.viewport_lines()).data;
    let records = table.records.len();
    if let Some(grid) = view.grid_mut() {
        grid.column = column;
        // Where the face will show the columns, kept, so a later move or press inside what
        // shows scrolls nothing.
        grid.left_column = if grid.cells_wide == 0 {
            grid.left_column.min(column)
        } else {
            let cells_wide = grid.cells_wide.max(MIN_PANEL_WIDTH);
            column_layout(
                &grid_widths(text, table, column, cells_wide),
                grid.left_column,
                false,
                column,
                cells_wide,
            )
            .0
        };
        grid.scrolled = false;
        let max_top = records.saturating_sub(data).max(1);
        let mut top = grid.top_record.clamp(1, max_top);
        if record >= 1 {
            if record < top {
                top = record;
            } else if record >= top.saturating_add(data) {
                top = record.saturating_add(1).saturating_sub(data);
            }
        }
        grid.top_record = top.clamp(1, max_top);
    }
}

/// A caret motion inside the draft.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Motion {
    Back,
    Forward,
    WordBack,
    WordForward,
    Up,
    Down,
    LineStart,
    LineEnd,
}

/// Where `motion` takes the caret at `caret` in `text`.
fn motion_target(text: &str, caret: usize, motion: Motion) -> usize {
    let caret = caret.min(text.len());
    match motion {
        Motion::Back => previous_boundary(text, caret),
        Motion::Forward => next_boundary(text, caret),
        Motion::WordBack => previous_word_boundary(text, caret, 1),
        Motion::WordForward => next_word_boundary(text, caret, 1),
        Motion::Up | Motion::Down => {
            let start = line_start(text, caret);
            let column = editor_display_column(
                text.get(start..line_text_end(text, caret))
                    .unwrap_or_default(),
                caret.saturating_sub(start),
                0,
            );
            if motion == Motion::Up {
                previous_line_position(text, caret, column)
            } else {
                next_line_position(text, caret, column)
            }
        }
        Motion::LineStart => line_start(text, caret),
        Motion::LineEnd => line_text_end(text, caret),
    }
}

fn draft_motion(view: &mut EditorBufferView, motion: Motion) {
    if let Some(draft) = view.grid_mut().and_then(|grid| grid.draft.as_mut()) {
        draft.caret = motion_target(&draft.text, draft.caret, motion);
        draft.end_run();
    }
}

/// Delete from the draft's caret to where `motion` takes it.
fn draft_delete(view: &mut EditorBufferView, motion: Motion) {
    if let Some(draft) = view.grid_mut().and_then(|grid| grid.draft.as_mut()) {
        let caret = draft.caret.min(draft.text.len());
        let target = motion_target(&draft.text, caret, motion);
        let range = caret.min(target)..caret.max(target);
        if range.is_empty() {
            return;
        }
        draft.begin(DraftRun::Delete);
        draft.text.replace_range(range.clone(), "");
        draft.caret = range.start;
    }
}

/// `C-k` in the draft: delete to the end of the line, or the line break at its end.
fn draft_kill_line(view: &mut EditorBufferView) {
    if let Some(draft) = view.grid_mut().and_then(|grid| grid.draft.as_mut()) {
        let caret = draft.caret.min(draft.text.len());
        let end = line_text_end(&draft.text, caret);
        let end = if end == caret {
            next_boundary(&draft.text, caret)
        } else {
            end
        };
        if end == caret {
            return;
        }
        draft.begin(DraftRun::Delete);
        draft.text.replace_range(caret..end, "");
    }
}

/// Undo (⌘Z, C-/, C-x u) or redo (⌘⇧Z, ⌘Y, C-?) the draft's last run of typing or deleting, a
/// paste on its own; undone back to the start, the draft holds the cell's own value. The
/// document's own history waits until the cell is kept or dropped.
fn draft_history(view: &mut EditorBufferView, undo: bool) {
    if let Some(draft) = view.grid_mut().and_then(|grid| grid.draft.as_mut()) {
        draft.step_history(undo);
    }
}

/// Close the cell editor without writing.
fn draft_drop(view: &mut EditorBufferView) {
    if let Some(grid) = view.grid_mut() {
        grid.draft = None;
    }
}

#[cfg(test)]
#[path = "grid_tests.rs"]
mod tests;
