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
use crate::store::{DocumentSnapshot, DocumentStore, EditDelta, rebase_position};
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
            EditorCommand::Undo | EditorCommand::Redo => {
                self.grid_history(store, view, command == EditorCommand::Undo)
            }
            EditorCommand::RecenterTopBottom => {
                let snapshot = snapshot_for_view(store, view)?;
                if let Ok(table) = self.grid_table(&snapshot, grid_delimiter(view)) {
                    let (record, _) = current_cell(&table, view);
                    let data = GridRows::of(view.viewport_lines()).data;
                    if let Some(grid) = view.grid_mut() {
                        grid.top_record = record.saturating_sub(data / 2).max(1);
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

    /// Undo or redo the document's last change, then put the cursor on the cell it touched.
    /// The history's own words (a frame that was not yours) come back as its effects.
    fn grid_history(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
        undo: bool,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        let effects = if undo {
            self.undo(store, view)?
        } else {
            self.redo(store, view)?
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
        let shown = table::clip(&value, 32);
        // A cell is a whole value: a copy never joins the kill before it.
        self.record_kill(value.clone(), None, false);
        *self.kills_mut().1 = Some(value);
        self.last_yank = None;
        Ok(vec![EditorEffect::Status(format!("Copied “{shown}”"))])
    }

    /// `kill-region` and `kill-line` (C-w, C-k, ⌘X): the current cell's value onto the kill ring,
    /// then the cell cleared, as one undo step.
    fn grid_cut(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
    ) -> Result<Vec<EditorEffect>, EditorError> {
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
mod tests {
    use super::*;
    use crate::editor::{CloseWhy, Invocation, Keymap, Platform};
    use crate::store::{DocumentId, DocumentViewId};

    /// Whether `effects` asks the host to close the view for `why`.
    fn closes(effects: &[EditorEffect], why: CloseWhy) -> bool {
        effects.contains(&EditorEffect::CloseView { why })
    }
    use crate::table::fixtures::{ALL_CRLF, MAIN, MIXED, STRAY};

    fn grid_editor(text: &str) -> (DocumentStore, EditorWorkspace, EditorBufferView, DocumentId) {
        grid_editor_named("file:///tmp/accounts.csv", text)
    }

    fn grid_editor_named(
        uri: &str,
        text: &str,
    ) -> (DocumentStore, EditorWorkspace, EditorBufferView, DocumentId) {
        let mut store = DocumentStore::for_test();
        let document = store.open(uri.into(), text.into());
        let mut workspace =
            EditorWorkspace::with_keymap(Keymap::emacs(Profile::Gui(Platform::Mac)));
        let mut view = workspace
            .attach(&mut store, document, DocumentViewId(31))
            .unwrap();
        view.reconcile_viewport(text, 20);
        workspace
            .set_face(&store, &mut view, Some(Delimiter::Comma))
            .unwrap();
        (store, workspace, view, document)
    }

    fn key(key: Key, mods: Modifiers) -> EditorInput {
        EditorInput::Key {
            key,
            mods,
            text: None,
        }
    }

    fn named(named: NamedKey) -> EditorInput {
        key(Key::Named(named), Modifiers::empty())
    }

    fn ch(character: char, mods: Modifiers) -> EditorInput {
        key(Key::Character(character), mods)
    }

    fn text_of(store: &DocumentStore, document: DocumentId) -> String {
        store.snapshot(document).unwrap().text.to_string()
    }

    fn cell_of(
        store: &DocumentStore,
        workspace: &mut EditorWorkspace,
        view: &EditorBufferView,
    ) -> (usize, usize) {
        let snapshot = store.snapshot(view.document).unwrap();
        let table = workspace.grid_table(&snapshot, Delimiter::Comma).unwrap();
        current_cell(&table, view)
    }

    fn press(
        store: &mut DocumentStore,
        workspace: &mut EditorWorkspace,
        view: &mut EditorBufferView,
        inputs: &[EditorInput],
    ) -> Vec<EditorEffect> {
        let mut effects = Vec::new();
        for input in inputs {
            effects = workspace
                .handle_input(store, view, input.clone())
                .unwrap()
                .effects;
        }
        effects
    }

    fn goto(
        store: &mut DocumentStore,
        workspace: &mut EditorWorkspace,
        view: &mut EditorBufferView,
        record: usize,
        column: usize,
    ) {
        press(
            store,
            workspace,
            view,
            &[EditorInput::GridPointer {
                record,
                column,
                open: false,
            }],
        );
        assert_eq!(cell_of(store, workspace, view), (record, column));
    }

    #[test]
    fn a_table_opens_on_the_cell_its_caret_is_in() {
        let (store, mut workspace, view, _) = grid_editor(MAIN);
        assert!(view.grid().is_some());
        assert_eq!(cell_of(&store, &mut workspace, &view), (0, 0));
        let big = format!(
            "a,b\n{}",
            "x,y\n".repeat(crate::table::MAX_TABLE_RECORDS + 1)
        );
        let mut store = DocumentStore::for_test();
        let document = store.open("file:///tmp/big.csv".into(), big);
        let mut workspace = EditorWorkspace::new();
        let mut view = workspace
            .attach(&mut store, document, DocumentViewId(3))
            .unwrap();
        let refusal = workspace
            .set_face(&store, &mut view, Some(Delimiter::Comma))
            .unwrap_err();
        assert!(refusal.starts_with("Opened as text"), "{refusal}");
        assert!(view.grid().is_none());
    }

    /// T5: every G1 key, from a known cursor, gives the documented cursor, draft and effects.
    #[test]
    fn t5_the_reducer_table() {
        use Modifiers as M;
        let none = M::empty();
        // (name, start, inputs, cursor after, draft text after)
        type Row<'a> = (
            &'a str,
            (usize, usize),
            Vec<EditorInput>,
            (usize, usize),
            Option<&'a str>,
        );
        let rows: &[Row<'_>] = &[
            (
                "right",
                (1, 1),
                vec![named(NamedKey::ArrowRight)],
                (1, 2),
                None,
            ),
            (
                "left",
                (1, 1),
                vec![named(NamedKey::ArrowLeft)],
                (1, 0),
                None,
            ),
            (
                "down",
                (1, 1),
                vec![named(NamedKey::ArrowDown)],
                (2, 1),
                None,
            ),
            ("up", (1, 1), vec![named(NamedKey::ArrowUp)], (0, 1), None),
            ("C-f", (1, 1), vec![ch('f', M::CTRL)], (1, 2), None),
            ("C-b", (1, 1), vec![ch('b', M::CTRL)], (1, 0), None),
            ("C-n", (1, 1), vec![ch('n', M::CTRL)], (2, 1), None),
            ("C-p", (1, 1), vec![ch('p', M::CTRL)], (0, 1), None),
            (
                "up stops at the header",
                (0, 1),
                vec![named(NamedKey::ArrowUp)],
                (0, 1),
                None,
            ),
            (
                "down stops at the end",
                (4, 1),
                vec![named(NamedKey::ArrowDown)],
                (4, 1),
                None,
            ),
            ("tab", (1, 1), vec![named(NamedKey::Tab)], (1, 2), None),
            (
                "tab wraps",
                (1, 6),
                vec![named(NamedKey::Tab)],
                (2, 0),
                None,
            ),
            (
                "S-tab wraps",
                (2, 0),
                vec![key(Key::Named(NamedKey::Tab), M::SHIFT)],
                (1, 6),
                None,
            ),
            ("home", (1, 3), vec![named(NamedKey::Home)], (1, 0), None),
            ("end", (1, 0), vec![named(NamedKey::End)], (1, 5), None),
            ("C-a", (1, 3), vec![ch('a', M::CTRL)], (1, 0), None),
            (
                "C-e on a short row",
                (3, 0),
                vec![ch('e', M::CTRL)],
                (3, 5),
                None,
            ),
            (
                "C-e on a long row",
                (4, 0),
                vec![ch('e', M::CTRL)],
                (4, 6),
                None,
            ),
            ("M->", (1, 2), vec![ch('>', M::ALT)], (4, 2), None),
            ("M-<", (3, 2), vec![ch('<', M::ALT)], (0, 2), None),
            (
                "page down",
                (0, 1),
                vec![named(NamedKey::PageDown)],
                (4, 1),
                None,
            ),
            (
                "page up",
                (4, 1),
                vec![named(NamedKey::PageUp)],
                (0, 1),
                None,
            ),
            ("C-v", (0, 1), vec![ch('v', M::CTRL)], (4, 1), None),
            ("M-v", (4, 1), vec![ch('v', M::ALT)], (0, 1), None),
            (
                "C-u 2 C-n",
                (0, 1),
                vec![ch('u', M::CTRL), ch('2', none), ch('n', M::CTRL)],
                (2, 1),
                None,
            ),
            (
                "return opens",
                (1, 2),
                vec![named(NamedKey::Enter)],
                (1, 2),
                Some("Claude"),
            ),
            (
                "F2 opens",
                (2, 2),
                vec![named(NamedKey::F2)],
                (2, 2),
                Some("Claude, GitHub"),
            ),
            (
                "typing replaces",
                (1, 2),
                vec![ch('x', none)],
                (1, 2),
                Some("x"),
            ),
            (
                "space replaces",
                (1, 2),
                vec![named(NamedKey::Space)],
                (1, 2),
                Some(" "),
            ),
            (
                "a missing cell opens empty",
                (3, 5),
                vec![named(NamedKey::Enter)],
                (3, 5),
                Some(""),
            ),
            (
                "escape drops",
                (1, 2),
                vec![ch('x', none), named(NamedKey::Escape)],
                (1, 2),
                None,
            ),
            (
                "C-g drops",
                (1, 2),
                vec![ch('x', none), ch('g', M::CTRL)],
                (1, 2),
                None,
            ),
            (
                "draft edits",
                (1, 2),
                vec![
                    named(NamedKey::Enter),
                    named(NamedKey::Backspace),
                    ch('e', none),
                    ch('a', M::CTRL),
                    ch('d', M::CTRL),
                    ch('X', none),
                ],
                (1, 2),
                Some("Xlaude"),
            ),
            (
                "M-RET breaks a line",
                (1, 2),
                vec![
                    ch('a', none),
                    key(Key::Named(NamedKey::Enter), M::ALT),
                    ch('b', none),
                ],
                (1, 2),
                Some("a\nb"),
            ),
            (
                "C-k kills to the line end",
                (1, 2),
                vec![
                    named(NamedKey::Enter),
                    ch('a', M::CTRL),
                    ch('f', M::CTRL),
                    ch('k', M::CTRL),
                ],
                (1, 2),
                Some("C"),
            ),
            (
                "⌘Z restores the draft",
                (1, 2),
                vec![named(NamedKey::Enter), ch('z', none), ch('z', M::SUPER)],
                (1, 2),
                Some("Claude"),
            ),
            (
                "⌘E is refused while editing",
                (1, 2),
                vec![ch('q', none), ch('e', M::SUPER)],
                (1, 2),
                Some("q"),
            ),
            (
                "C-g cancels a prefix",
                (1, 2),
                vec![ch('x', M::CTRL), ch('g', M::CTRL)],
                (1, 2),
                None,
            ),
        ];
        for (name, start, inputs, expected, draft) in rows {
            let (mut store, mut workspace, mut view, document) = grid_editor(MAIN);
            goto(&mut store, &mut workspace, &mut view, start.0, start.1);
            let before = store.snapshot(document).unwrap().seq;
            press(&mut store, &mut workspace, &mut view, inputs);
            assert_eq!(cell_of(&store, &mut workspace, &view), *expected, "{name}");
            assert_eq!(
                view.grid_draft().map(|draft| draft.text.as_str()),
                *draft,
                "{name}"
            );
            assert_eq!(
                store.snapshot(document).unwrap().seq,
                before,
                "{name}: no edit"
            );
            assert!(view.grid().is_some(), "{name}");
        }
    }

    /// T5: Return commits one undo frame over the cell's bytes, Escape writes nothing, undo
    /// from a text view of the same document takes a grid commit back, and ⌘Z in the grid puts
    /// the cursor on the cell it changed.
    #[test]
    fn t5_commits_escape_and_one_shared_history() {
        let (mut store, mut workspace, mut view, document) = grid_editor(MAIN);
        goto(&mut store, &mut workspace, &mut view, 2, 5);
        let before = store.snapshot(document).unwrap().seq;
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[
                ch('n', Modifiers::empty()),
                ch('o', Modifiers::empty()),
                named(NamedKey::Escape),
            ],
        );
        assert_eq!(
            store.snapshot(document).unwrap().seq,
            before,
            "Escape writes nothing"
        );
        assert_eq!(text_of(&store, document), MAIN);
        assert!(!workspace.can_undo(document));

        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[EditorInput::Text("fix, ".into())],
        );
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[EditorInput::Paste("\"quoted\"".into())],
        );
        assert_eq!(view.grid_draft().unwrap().text, "fix, \"quoted\"");
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[named(NamedKey::Enter)],
        );
        let committed = text_of(&store, document);
        assert_eq!(
            committed,
            MAIN.replace(
                "then the code\",\n",
                "then the code\",\"fix, \"\"quoted\"\"\"\n"
            )
        );
        assert_eq!(
            store.snapshot(document).unwrap().seq.0,
            before.0 + 1,
            "one transaction"
        );
        assert_eq!(
            cell_of(&store, &mut workspace, &view),
            (3, 5),
            "Return moves down"
        );
        assert!(view.grid_draft().is_none());

        // Committing a cell's own value is no edit, no undo step and no dirty mark.
        let seq = store.snapshot(document).unwrap().seq;
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[
                named(NamedKey::ArrowUp),
                named(NamedKey::Enter),
                named(NamedKey::Enter),
            ],
        );
        assert_eq!(store.snapshot(document).unwrap().seq, seq);

        // Undo from a text view of the same document takes the grid's commit back.
        let mut text_view = workspace
            .attach(&mut store, document, DocumentViewId(32))
            .unwrap();
        workspace
            .execute(&mut store, &mut text_view, EditorCommand::Undo)
            .unwrap();
        assert_eq!(text_of(&store, document), MAIN);
        for (seq, deltas) in workspace.take_commits(document) {
            view.observe_external(seq, &deltas);
        }
        // Redo from the grid, and ⌘Z there: the cursor lands on the changed cell.
        goto(&mut store, &mut workspace, &mut view, 0, 0);
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[ch('z', Modifiers::SUPER | Modifiers::SHIFT)],
        );
        assert_eq!(text_of(&store, document), committed);
        assert_eq!(cell_of(&store, &mut workspace, &view), (2, 5));
        goto(&mut store, &mut workspace, &mut view, 4, 0);
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[ch('z', Modifiers::SUPER)],
        );
        assert_eq!(text_of(&store, document), MAIN);
        assert_eq!(cell_of(&store, &mut workspace, &view), (2, 5));
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[ch('?', Modifiers::CTRL)],
        );
        assert_eq!(text_of(&store, document), committed, "C-? redoes");
    }

    #[test]
    fn delete_and_paste_are_one_undo_step_each() {
        let (mut store, mut workspace, mut view, document) = grid_editor(MAIN);
        goto(&mut store, &mut workspace, &mut view, 2, 2);
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[named(NamedKey::Backspace)],
        );
        assert_eq!(
            text_of(&store, document),
            MAIN.replace("\"Claude, GitHub\"", "\"\"")
        );
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[EditorInput::Paste("a,b\n".into())],
        );
        assert_eq!(
            text_of(&store, document),
            MAIN.replace("\"Claude, GitHub\"", "\"a,b\"")
        );
        assert_eq!(cell_of(&store, &mut workspace, &view), (2, 2));
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[ch('/', Modifiers::CTRL)],
        );
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[ch('x', Modifiers::CTRL), ch('u', Modifiers::empty())],
        );
        assert_eq!(text_of(&store, document), MAIN);
        let seq = store.snapshot(document).unwrap().seq;
        goto(&mut store, &mut workspace, &mut view, 1, 5);
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[named(NamedKey::Delete)],
        );
        assert_eq!(
            store.snapshot(document).unwrap().seq,
            seq,
            "clearing an empty cell is no edit"
        );
    }

    #[test]
    fn saving_finishing_and_flipping() {
        let (mut store, mut workspace, mut view, document) = grid_editor(MAIN);
        goto(&mut store, &mut workspace, &mut view, 1, 5);
        let effects = press(
            &mut store,
            &mut workspace,
            &mut view,
            &[ch('s', Modifiers::SUPER)],
        );
        assert!(matches!(
            effects.as_slice(),
            [EditorEffect::SaveDocument { .. }]
        ));
        let effects = press(
            &mut store,
            &mut workspace,
            &mut view,
            &[ch('x', Modifiers::CTRL), ch('s', Modifiers::CTRL)],
        );
        assert!(matches!(
            effects.as_slice(),
            [EditorEffect::SaveDocument { .. }]
        ));

        // ⌘S with a cell open commits it, then saves.
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[ch('y', Modifiers::empty())],
        );
        let effects = press(
            &mut store,
            &mut workspace,
            &mut view,
            &[ch('s', Modifiers::SUPER)],
        );
        assert!(matches!(
            effects.as_slice(),
            [EditorEffect::SaveDocument { .. }]
        ));
        assert!(text_of(&store, document).contains("so email.\",y\n"));

        // C-c C-c with a cell open commits, then finishes; C-c C-k drops it, then aborts.
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[named(NamedKey::ArrowRight), ch('z', Modifiers::empty())],
        );
        let effects = press(
            &mut store,
            &mut workspace,
            &mut view,
            &[ch('c', Modifiers::CTRL), ch('c', Modifiers::CTRL)],
        );
        assert!(closes(&effects, CloseWhy::Finish), "{effects:?}");
        let committed = text_of(&store, document);
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[ch('w', Modifiers::empty())],
        );
        let effects = press(
            &mut store,
            &mut workspace,
            &mut view,
            &[ch('c', Modifiers::CTRL), ch('k', Modifiers::CTRL)],
        );
        assert!(closes(&effects, CloseWhy::Abort), "{effects:?}");
        assert_eq!(text_of(&store, document), committed);

        // ⌘E and C-c C-t flip to the text and back, keeping the place.
        goto(&mut store, &mut workspace, &mut view, 2, 2);
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[ch('e', Modifiers::SUPER)],
        );
        assert!(view.grid().is_none());
        let head = view.primary_selection().head;
        assert_eq!(&committed[head..head + 6], "Claude");
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[ch('c', Modifiers::CTRL), ch('t', Modifiers::CTRL)],
        );
        assert_eq!(cell_of(&store, &mut workspace, &view), (2, 2));
        // A file that is not a table has no grid.
        let mut store = DocumentStore::for_test();
        let document = store.open("file:///tmp/notes.txt".into(), "a,b\n".into());
        let mut workspace = EditorWorkspace::new();
        let mut view = workspace
            .attach(&mut store, document, DocumentViewId(5))
            .unwrap();
        let effects = workspace
            .execute(&mut store, &mut view, EditorCommand::ToggleGridView)
            .unwrap();
        assert!(view.grid().is_none());
        assert!(
            matches!(effects.as_slice(), [EditorEffect::Status(words)] if words.starts_with("Not a table"))
        );
        // The terminal face shows a table as text, and says where the grid is.
        let mut store = DocumentStore::for_test();
        let document = store.open("file:///tmp/t.csv".into(), "a,b\n".into());
        let mut workspace = EditorWorkspace::with_keymap(Keymap::emacs(Profile::Terminal));
        let mut view = workspace
            .attach(&mut store, document, DocumentViewId(6))
            .unwrap();
        let effects = workspace
            .execute(&mut store, &mut view, EditorCommand::ToggleGridView)
            .unwrap();
        assert!(view.grid().is_none());
        assert_eq!(
            effects,
            vec![EditorEffect::Status(TERMINAL_TABLE.to_string())]
        );
    }

    #[test]
    fn search_puts_the_cursor_on_the_match() {
        let (mut store, mut workspace, mut view, _) = grid_editor(MAIN);
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[ch('s', Modifiers::CTRL)],
        );
        for character in "carol".chars() {
            press(
                &mut store,
                &mut workspace,
                &mut view,
                &[ch(character, Modifiers::empty())],
            );
        }
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[named(NamedKey::Enter)],
        );
        assert!(!view.minibuffer_active());
        assert_eq!(cell_of(&store, &mut workspace, &view), (3, 1));
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[named(NamedKey::ArrowRight)],
        );
        assert_eq!(cell_of(&store, &mut workspace, &view), (3, 2));
    }

    /// T6: a foreign commit to the drafted cell makes Return refuse and keep the draft; one to
    /// another cell lets the commit land on the right cell.
    #[test]
    fn t6_a_draft_commits_only_over_the_value_it_opened_with() {
        let (mut store, mut workspace, mut view, document) = grid_editor(MAIN);
        goto(&mut store, &mut workspace, &mut view, 2, 3);
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[named(NamedKey::Enter), ch('!', Modifiers::empty())],
        );
        let mut other = workspace
            .attach(&mut store, document, DocumentViewId(33))
            .unwrap();
        let sms = MAIN.find("SMS").unwrap();
        other.selections = vec![Selection::caret(sms)];
        workspace
            .insert_text(&mut store, &mut other, "MMS or ")
            .unwrap();
        for (seq, deltas) in workspace.take_commits(document) {
            view.observe_external(seq, &deltas);
        }
        let foreign = text_of(&store, document);
        let effects = press(
            &mut store,
            &mut workspace,
            &mut view,
            &[named(NamedKey::Enter)],
        );
        assert_eq!(effects, [EditorEffect::Status(CELL_CHANGED.to_string())]);
        assert_eq!(view.grid_draft().unwrap().text, "SMS!");
        assert_eq!(text_of(&store, document), foreign, "the foreign text stays");
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[named(NamedKey::Escape)],
        );

        // A foreign commit to another cell: the draft lands on its own cell.
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[named(NamedKey::Enter), ch('?', Modifiers::empty())],
        );
        other.selections = vec![Selection::caret(foreign.find("m4").unwrap())];
        workspace
            .insert_text(&mut store, &mut other, "big ")
            .unwrap();
        for (seq, deltas) in workspace.take_commits(document) {
            view.observe_external(seq, &deltas);
        }
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[named(NamedKey::Enter)],
        );
        let landed = text_of(&store, document);
        assert!(landed.contains(",MMS or SMS?,"), "{landed}");
        assert!(landed.contains("\nbig m4,"), "{landed}");
    }

    /// T7: in a grid, every command either is refused and leaves the text alone, or changes at
    /// most the current cell's bytes; a row command (G2) changes at most its own row's.
    #[test]
    fn t7_commands_that_edit_raw_text_are_refused() {
        for command in EditorCommand::ALL {
            let (mut store, mut workspace, mut view, document) = grid_editor(MAIN);
            goto(&mut store, &mut workspace, &mut view, 2, 2);
            workspace.kill_ring.push_front("zz".to_string());
            let snapshot = store.snapshot(document).unwrap();
            let table = Table::scan(&snapshot.text, Delimiter::Comma).unwrap();
            let rows = matches!(
                command,
                EditorCommand::OpenLine
                    | EditorCommand::GridInsertRow
                    | EditorCommand::GridDeleteRow
            );
            let own = if rows {
                table.records[2].raw.start..table.records[2].terminator.end
            } else {
                table.cell(2, 2).unwrap().raw.clone()
            };
            let result = workspace.invoke(&mut store, &mut view, Invocation::new(command));
            let after = text_of(&store, document);
            let changed_outside = after.len() + own.len() < MAIN.len()
                || after[..own.start] != MAIN[..own.start]
                || after[after.len() - (MAIN.len() - own.end)..] != MAIN[own.end..];
            assert!(
                !changed_outside,
                "{} edited outside its {}: {after:?}",
                command.name(),
                if rows { "row" } else { "cell" }
            );
            if !command.allowed_in_grid() {
                assert_eq!(after, MAIN, "{}", command.name());
                let effects = result.unwrap();
                assert!(
                    matches!(effects.as_slice(), [EditorEffect::Status(words)] if words.contains("not in the grid view")),
                    "{}: {effects:?}",
                    command.name()
                );
            }
            assert!(
                view.grid().is_some() || command == EditorCommand::ToggleGridView,
                "{}",
                command.name()
            );
        }
    }

    #[test]
    fn closing_commits_an_open_draft_first() {
        let (mut store, mut workspace, mut view, document) = grid_editor(MIXED);
        goto(&mut store, &mut workspace, &mut view, 1, 3);
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[EditorInput::Text("done".into())],
        );
        workspace
            .commit_before_close(&mut store, &mut view)
            .unwrap();
        assert!(view.grid_draft().is_none());
        assert_eq!(
            text_of(&store, document),
            MIXED.replace("and two\",\r\n", "and two\",done\r\n"),
            "the mixed file's CRLF records and LF cell are kept byte for byte"
        );
        // A refused commit refuses the close and keeps the text.
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[named(NamedKey::Enter), ch('!', Modifiers::empty())],
        );
        let mut other = workspace
            .attach(&mut store, document, DocumentViewId(34))
            .unwrap();
        other.selections = vec![Selection::caret(
            text_of(&store, document).find("done").unwrap(),
        )];
        workspace.insert_text(&mut store, &mut other, "un").unwrap();
        for (seq, deltas) in workspace.take_commits(document) {
            view.observe_external(seq, &deltas);
        }
        assert_eq!(
            workspace.commit_before_close(&mut store, &mut view),
            Err(CELL_CHANGED.to_string())
        );
        assert_eq!(view.grid_draft().unwrap().text, "done!");
    }

    #[test]
    fn the_projection_lays_out_header_rows_and_panel() {
        let (mut store, mut workspace, mut view, _) = grid_editor(MAIN);
        goto(&mut store, &mut workspace, &mut view, 2, 4);
        let snapshot = store.snapshot(view.document).unwrap();
        let table = Table::scan(&snapshot.text, Delimiter::Comma).unwrap();
        let projection = project_grid(&snapshot.text, &table, &view, 20, 60);
        assert_eq!(projection.records, 5);
        assert_eq!(projection.columns, 7);
        assert_eq!(projection.cursor.record, 2);
        assert_eq!(projection.cursor.column, 4);
        assert_eq!(projection.cursor.line, 3);
        assert!(
            projection.shown.iter().any(|column| column.index == 4),
            "the cursor's column shows"
        );
        assert!(projection.hidden_left);
        assert_eq!(projection.header.record, 0);
        assert_eq!(projection.rows.first().map(|row| row.record), Some(1));
        let row3 = projection.rows.iter().find(|row| row.record == 3).unwrap();
        let shown_five = projection.shown.iter().position(|column| column.index == 5);
        if let Some(index) = shown_five {
            assert!(row3.cells[index].missing, "m3 lacks column 6");
        }
        let at = projection
            .shown
            .iter()
            .position(|column| column.index == 4)
            .unwrap();
        let drawn = &projection
            .rows
            .iter()
            .find(|row| row.record == 2)
            .unwrap()
            .cells[at]
            .text;
        assert!(drawn.ends_with('…') || drawn.ends_with('⏎'), "{drawn}");
        assert_eq!(projection.panel.label, "Row 3 · why I guessed");
        assert_eq!(
            projection.panel.lines.first().map(String::as_str),
            Some("Two reasons: first")
        );
        assert_eq!(projection.panel.caret, None);

        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[named(NamedKey::Enter)],
        );
        let projection = project_grid(&snapshot.text, &table, &view, 20, 60);
        assert!(projection.panel.editing);
        assert_eq!(
            projection.panel.caret,
            Some((1, "the phone prompt, then the code".len()))
        );
        assert_eq!(GridRows::of(20), GridRows { data: 12, panel: 6 });
        assert_eq!(GridRows::of(60), GridRows { data: 50, panel: 8 });
        assert_eq!(GridRows::of(1), GridRows { data: 1, panel: 1 });
    }

    /// A grid told its width keeps its columns where the face shows them: a move or a press back
    /// inside what shows scrolls nothing sideways (the lab saw a click on a shown cell jump the
    /// columns under the pointer). And an open cell shows what is typed into it.
    #[test]
    fn the_columns_stay_where_they_show_and_an_open_cell_shows_its_draft() {
        let (mut store, mut workspace, mut view, _) = grid_editor(MAIN);
        // Wide enough for the last two columns, not the last three.
        let cells = 100;
        assert!(view.set_grid_width(cells));
        assert!(!view.set_grid_width(cells), "unchanged");
        // The first scrolled column, after the pinned first column once the grid has scrolled.
        let first_shown = |store: &DocumentStore, view: &EditorBufferView| {
            let snapshot = store.snapshot(view.document).unwrap();
            let table = Table::scan(&snapshot.text, Delimiter::Comma).unwrap();
            let projection = project_grid(&snapshot.text, &table, view, 20, cells);
            projection.shown[usize::from(projection.pinned)].index
        };
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[named(NamedKey::End)],
        );
        let scrolled = first_shown(&store, &view);
        assert!(scrolled > 0, "the last column scrolls the grid");
        assert_eq!(view.grid().unwrap().left_column, scrolled);
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[named(NamedKey::ArrowLeft)],
        );
        assert_eq!(first_shown(&store, &view), scrolled, "a move inside stays");
        workspace
            .handle_input(
                &mut store,
                &mut view,
                EditorInput::GridPointer {
                    record: 2,
                    column: scrolled,
                    open: false,
                },
            )
            .unwrap();
        assert_eq!(
            first_shown(&store, &view),
            scrolled,
            "a press on a shown cell stays"
        );

        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[ch('z', Modifiers::empty())],
        );
        let snapshot = store.snapshot(view.document).unwrap();
        let table = Table::scan(&snapshot.text, Delimiter::Comma).unwrap();
        let projection = project_grid(&snapshot.text, &table, &view, 20, cells);
        let at = projection
            .shown
            .iter()
            .position(|shown| shown.index == scrolled)
            .unwrap();
        let row = projection.rows.iter().find(|row| row.record == 2).unwrap();
        assert_eq!(row.cells[at].text, "z");
        assert_eq!(
            projection.draft.as_ref().map(|draft| draft.text.as_str()),
            Some("z")
        );
    }

    #[test]
    fn the_wheel_scrolls_rows_and_leaves_the_cursor() {
        let text = format!(
            "h1,h2\n{}",
            (0..200).map(|n| format!("r{n},v{n}\n")).collect::<String>()
        );
        let (mut store, mut workspace, mut view, _) = grid_editor(&text);
        workspace.scroll_view(&store, &mut view, 30).unwrap();
        assert_eq!(view.grid().unwrap().top_record, 31);
        assert_eq!(cell_of(&store, &mut workspace, &view), (0, 0));
        workspace.scroll_view(&store, &mut view, 10_000).unwrap();
        assert_eq!(view.grid().unwrap().top_record, 201 - GridRows::of(20).data);
        workspace.scroll_view(&store, &mut view, -10_000).unwrap();
        assert_eq!(view.grid().unwrap().top_record, 1);
        goto(&mut store, &mut workspace, &mut view, 150, 1);
        let top = view.grid().unwrap().top_record;
        assert!(top <= 150 && 150 < top + GridRows::of(20).data, "{top}");
        // A TSV document splits on tabs.
        let (store, mut workspace, mut view, _) =
            grid_editor_named("file:///tmp/x.tsv", "a\tb,c\n");
        workspace.set_face(&store, &mut view, None).unwrap();
        workspace
            .set_face(&store, &mut view, Some(Delimiter::Tab))
            .unwrap();
        let snapshot = store.snapshot(view.document).unwrap();
        let table = workspace.grid_table(&snapshot, Delimiter::Tab).unwrap();
        assert_eq!(table.records[0].cells.len(), 2);
    }

    /// The owner's shape with CRLF between records, an LF inside a quoted cell, a short row (m3)
    /// and no final newline; m5's correction cell is the last of the text.
    const RACE: &str = concat!(
        "computer,AI login email,services,codes arrive in (my guess),why I guessed,your correction (leave blank if my guess is right)\r\n",
        "m1,alice@example.test,Claude,email,\"The sign-in page said \"\"check your inbox\"\", so email.\",\r\n",
        "m2,bob@example.test,\"Claude, GitHub\",SMS,\"two\nlines\",\r\n",
        "m3,carol@example.test,Claude\r\n",
        "m5,erin@example.test,Claude,SMS,phone on file,",
    );

    /// The file on disk becomes `text` and the buffer follows it as the editor does: one reload
    /// frame through the workspace's writer, which the view hears as the GUI publishes it.
    fn rewrite(
        store: &mut DocumentStore,
        workspace: &mut EditorWorkspace,
        view: &mut EditorBufferView,
        document: DocumentId,
        text: &str,
    ) {
        let base = store.snapshot(document).unwrap().seq;
        let (seq, deltas) = workspace
            .replace_text(store, document, base, text, crate::editor::Origin::Disk)
            .expect("the reload commits");
        assert!(!deltas.is_empty(), "the reload changed the text");
        view.observe_external(seq, &deltas);
    }

    /// Open `cell`, type `typed`, and return the status a first Return gives.
    fn type_into(
        store: &mut DocumentStore,
        workspace: &mut EditorWorkspace,
        view: &mut EditorBufferView,
        cell: (usize, usize),
        typed: &str,
    ) {
        goto(store, workspace, view, cell.0, cell.1);
        press(store, workspace, view, &[EditorInput::Text(typed.into())]);
        assert_eq!(view.grid_draft().unwrap().text, typed);
    }

    /// A draft is tied to its own cell, not to the caret or to a value that matches: when another
    /// writer changes the rows around it, the commit writes nothing until the person has seen
    /// which cell it would write; a change elsewhere lets it land on its own cell at once. (The
    /// review saw an empty correction land in a row an agent appended, in a deleted row's
    /// neighbour, and in a column appended to every row: empty matched empty there.)
    #[test]
    fn a_draft_lands_only_in_its_own_cell_when_the_rows_move() {
        let enter = [named(NamedKey::Enter)];
        // A row appended after the drafted last row (no final newline: the append starts at the
        // cell's own end, where the caret sits).
        let (mut store, mut workspace, mut view, document) = grid_editor(RACE);
        type_into(&mut store, &mut workspace, &mut view, (4, 5), "fix");
        let appended = format!("{RACE}\r\nm9,zed@example.test,Claude,email,added by an agent,");
        rewrite(&mut store, &mut workspace, &mut view, document, &appended);
        let effects = press(&mut store, &mut workspace, &mut view, &enter);
        assert!(
            matches!(effects.as_slice(), [EditorEffect::Status(words)] if words.contains("row 5 (m5)")),
            "{effects:?}"
        );
        assert_eq!(text_of(&store, document), appended, "nothing written yet");
        assert_eq!(view.grid_draft().unwrap().text, "fix", "the text is kept");
        assert_eq!(cell_of(&store, &mut workspace, &view), (4, 5));
        press(&mut store, &mut workspace, &mut view, &enter);
        assert_eq!(
            text_of(&store, document),
            appended.replace("phone on file,\r\n", "phone on file,fix\r\n"),
            "the second Return writes m5's cell, and m9 keeps its own"
        );

        // A column appended to every row: the draft stays on its own column.
        let (mut store, mut workspace, mut view, document) = grid_editor(RACE);
        type_into(&mut store, &mut workspace, &mut view, (2, 5), "fix");
        let widened = RACE
            .split("\r\n")
            .map(|row| format!("{row},"))
            .collect::<Vec<_>>()
            .join("\r\n");
        rewrite(&mut store, &mut workspace, &mut view, document, &widened);
        let effects = press(&mut store, &mut workspace, &mut view, &enter);
        assert!(
            matches!(effects.as_slice(), [EditorEffect::Status(words)] if words.contains("row 3 (m2) · your correction")),
            "{effects:?}"
        );
        assert_eq!(text_of(&store, document), widened);
        press(&mut store, &mut workspace, &mut view, &enter);
        assert!(
            text_of(&store, document).contains("\"two\nlines\",fix,\r\n"),
            "{:?}",
            text_of(&store, document)
        );

        // The drafted last row deleted, or replaced in place by another machine's row: the
        // commit refuses and names the cell a second Return would write.
        let with_newline = format!("{RACE}\r\n");
        let last_row = "m5,erin@example.test,Claude,SMS,phone on file,\r\n";
        for rewritten in [
            with_newline.replace(last_row, ""),
            with_newline.replace(
                last_row,
                "m7,gus@example.test,Claude,email,other machine,\r\n",
            ),
        ] {
            let (mut store, mut workspace, mut view, document) = grid_editor(&with_newline);
            type_into(&mut store, &mut workspace, &mut view, (4, 5), "fix");
            rewrite(&mut store, &mut workspace, &mut view, document, &rewritten);
            let effects = press(&mut store, &mut workspace, &mut view, &enter);
            assert!(
                matches!(effects.as_slice(), [EditorEffect::Status(words)] if words.starts_with("The file changed around this cell")),
                "{rewritten:?}: {effects:?}"
            );
            assert_eq!(text_of(&store, document), rewritten, "nothing written");
            assert_eq!(view.grid_draft().unwrap().text, "fix");
        }

        // A change wholly before the drafted row moves it and nothing more: the commit lands
        // on its own cell at once.
        let (mut store, mut workspace, mut view, document) = grid_editor(RACE);
        type_into(&mut store, &mut workspace, &mut view, (4, 5), "fix");
        let earlier = RACE.replace("alice@", "alice.b@");
        rewrite(&mut store, &mut workspace, &mut view, document, &earlier);
        let effects = press(&mut store, &mut workspace, &mut view, &enter);
        assert!(effects.is_empty(), "{effects:?}");
        assert_eq!(
            text_of(&store, document),
            earlier.replace("phone on file,", "phone on file,fix")
        );
    }

    /// A cell whose quote never closes holds every later row: the grid neither opens, clears,
    /// pastes into nor commits it, so no key drops those rows (the review saw one Backspace
    /// empty the rest of the file).
    #[test]
    fn a_cell_that_never_closes_is_not_changed() {
        const UNCLOSED: &str = concat!(
            "computer,email,services,why I guessed,fix\n",
            "m1,alice@example.test,Claude,\"5 inch screen, so SMS,\n",
            "m2,bob@example.test,Claude,plain,\n",
            "m3,carol@example.test,Claude,plain,\n",
        );
        let (mut store, mut workspace, mut view, document) = grid_editor(UNCLOSED);
        goto(&mut store, &mut workspace, &mut view, 1, 3);
        let refused = |effects: &[EditorEffect]| matches!(effects, [EditorEffect::Status(words)] if words == CELL_UNCLOSED);
        for input in [
            named(NamedKey::Backspace),
            named(NamedKey::Delete),
            EditorInput::Paste("x".into()),
            ch('x', Modifiers::empty()),
            named(NamedKey::Enter),
        ] {
            let effects = press(
                &mut store,
                &mut workspace,
                &mut view,
                std::slice::from_ref(&input),
            );
            assert!(refused(&effects), "{input:?}: {effects:?}");
            assert_eq!(text_of(&store, document), UNCLOSED, "{input:?}");
            assert!(view.grid_draft().is_none(), "{input:?}");
        }
        // The cells before it still edit in place.
        goto(&mut store, &mut workspace, &mut view, 1, 2);
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[EditorInput::Text("Codex".into()), named(NamedKey::Enter)],
        );
        assert_eq!(
            text_of(&store, document),
            UNCLOSED.replace("test,Claude,\"5", "test,Codex,\"5")
        );
    }

    /// The cell editor has its own undo and redo: ⌘Z takes back the last run of typing, ⌘⇧Z and
    /// ⌘Y bring it back, and neither ever wipes what was typed (the review saw ⌘⇧Z empty a typed
    /// correction).
    #[test]
    fn the_cell_editor_undoes_and_redoes_its_own_typing() {
        let (mut store, mut workspace, mut view, _) = grid_editor(MAIN);
        let draft = |view: &EditorBufferView| view.grid_draft().unwrap().text.clone();
        let cmd = |character: char| ch(character, Modifiers::SUPER);
        let cmd_shift = |character: char| ch(character, Modifiers::SUPER | Modifiers::SHIFT);
        // Typing replaces "email": ⌘⇧Z with nothing undone keeps it, ⌘Z gives the value back.
        goto(&mut store, &mut workspace, &mut view, 1, 3);
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[EditorInput::Text("typed a long reason".into())],
        );
        press(&mut store, &mut workspace, &mut view, &[cmd_shift('z')]);
        assert_eq!(
            draft(&view),
            "typed a long reason",
            "⌘⇧Z with nothing undone"
        );
        press(&mut store, &mut workspace, &mut view, &[cmd('z')]);
        assert_eq!(draft(&view), "email");
        press(&mut store, &mut workspace, &mut view, &[cmd_shift('z')]);
        assert_eq!(draft(&view), "typed a long reason", "⌘⇧Z redoes");
        // A caret move ends a run: two runs are two undos; ⌘Y redoes, C-/ undoes.
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[
                named(NamedKey::Home),
                ch('A', Modifiers::empty()),
                ch(':', Modifiers::empty()),
                named(NamedKey::End),
                named(NamedKey::Backspace),
                named(NamedKey::Backspace),
            ],
        );
        assert_eq!(draft(&view), "A:typed a long reas");
        press(&mut store, &mut workspace, &mut view, &[cmd('z')]);
        assert_eq!(
            draft(&view),
            "A:typed a long reason",
            "the deletes are one step"
        );
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[ch('/', Modifiers::CTRL)],
        );
        assert_eq!(draft(&view), "typed a long reason", "C-/ undoes too");
        press(&mut store, &mut workspace, &mut view, &[cmd('y')]);
        assert_eq!(draft(&view), "A:typed a long reason", "⌘Y redoes");
        // A paste is its own step; a new edit ends what redo could bring back.
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[EditorInput::Paste("!".into()), cmd('z')],
        );
        assert_eq!(draft(&view), "A:typed a long reason");
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[ch('?', Modifiers::empty()), cmd_shift('z')],
        );
        assert_eq!(
            draft(&view),
            "A:typed a long reason?",
            "nothing left to redo"
        );
        // Opened on its value (Return), undo back to the start is the value.
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[named(NamedKey::Escape)],
        );
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[
                named(NamedKey::Enter),
                ch('!', Modifiers::empty()),
                cmd('z'),
                cmd('z'),
            ],
        );
        assert_eq!(draft(&view), "email");
    }

    /// The owner's file in a window 110 cells wide: on the correction column the grid still
    /// shows which computer and which guess the row is (the first column pinned, long columns
    /// capped to a quarter of the row), and a sideways scroll reaches every column with the
    /// cursor left where it was.
    #[test]
    fn the_first_column_stays_pinned_and_the_columns_scroll_sideways() {
        let cells = 110;
        let (mut store, mut workspace, mut view, _) = grid_editor(RACE);
        view.set_grid_width(cells);
        let projection = |store: &DocumentStore, view: &EditorBufferView| {
            let snapshot = store.snapshot(view.document).unwrap();
            let table = Table::scan(&snapshot.text, Delimiter::Comma).unwrap();
            project_grid(&snapshot.text, &table, view, 20, cells)
        };
        let shown = |store: &DocumentStore, view: &EditorBufferView| {
            projection(store, view)
                .shown
                .iter()
                .map(|shown| shown.index)
                .collect::<Vec<_>>()
        };
        goto(&mut store, &mut workspace, &mut view, 1, 0);
        assert!(
            !projection(&store, &view).pinned,
            "nothing to pin at the start"
        );
        assert!(projection(&store, &view).hidden_right);
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[named(NamedKey::End)],
        );
        let end = projection(&store, &view);
        assert!(end.pinned, "{:?}", shown(&store, &view));
        assert_eq!(shown(&store, &view), vec![0, 3, 4, 5]);
        assert!(end.hidden_left && !end.hidden_right);
        assert!(
            end.shown.iter().all(|shown| shown.width <= cells / 4),
            "no column takes more than a quarter of the row: {:?}",
            end.shown
        );
        // Left and a press on a shown cell keep the columns where they show.
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[named(NamedKey::ArrowLeft)],
        );
        assert_eq!(shown(&store, &view), vec![0, 3, 4, 5]);
        goto(&mut store, &mut workspace, &mut view, 2, 3);
        assert_eq!(shown(&store, &view), vec![0, 3, 4, 5]);

        // From the first column, sideways scrolls reach the last and come back; the cursor stays.
        goto(&mut store, &mut workspace, &mut view, 2, 0);
        let start = shown(&store, &view);
        let scroll = |store: &mut DocumentStore,
                      workspace: &mut EditorWorkspace,
                      view: &mut EditorBufferView,
                      columns: i32| {
            press(
                store,
                workspace,
                view,
                &[EditorInput::GridScroll { columns }],
            );
        };
        scroll(&mut store, &mut workspace, &mut view, 1);
        let once = shown(&store, &view);
        assert_ne!(once, start, "the first step shows a column it hid");
        scroll(&mut store, &mut workspace, &mut view, 1);
        assert_eq!(shown(&store, &view), vec![0, 3, 4, 5]);
        scroll(&mut store, &mut workspace, &mut view, 5);
        assert_eq!(
            shown(&store, &view),
            vec![0, 3, 4, 5],
            "no further than the last"
        );
        assert_eq!(cell_of(&store, &mut workspace, &view), (2, 0));
        scroll(&mut store, &mut workspace, &mut view, -1);
        assert_eq!(shown(&store, &view), once);
        scroll(&mut store, &mut workspace, &mut view, -1);
        assert_eq!(shown(&store, &view), start);
        // Scrolled away from the cursor, a move brings its column back.
        scroll(&mut store, &mut workspace, &mut view, 2);
        goto(&mut store, &mut workspace, &mut view, 2, 1);
        assert!(shown(&store, &view).contains(&1));
        // With a cell open, a sideways scroll keeps the open cell in view.
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[named(NamedKey::Enter)],
        );
        let editing = shown(&store, &view);
        scroll(&mut store, &mut workspace, &mut view, 3);
        assert_eq!(shown(&store, &view), editing);
    }

    /// The panel is as tall as the current cell wraps to, so a short cell leaves the rows to the
    /// grid; a move keeps the cursor inside the rows the tallest panel leaves, so a long cell's
    /// panel never covers its row.
    #[test]
    fn the_panel_fits_its_cell_and_never_covers_the_cursor() {
        let long = "word ".repeat(100);
        let text = format!(
            "h1,h2\n{}",
            (0..60)
                .map(|n| format!("r{n},{}\n", if n == 30 { long.as_str() } else { "v" }))
                .collect::<String>()
        );
        let (mut store, mut workspace, mut view, _) = grid_editor(&text);
        view.set_grid_width(60);
        let project = |store: &DocumentStore, view: &EditorBufferView| {
            let snapshot = store.snapshot(view.document).unwrap();
            let table = Table::scan(&snapshot.text, Delimiter::Comma).unwrap();
            project_grid(&snapshot.text, &table, view, 20, 60)
        };
        goto(&mut store, &mut workspace, &mut view, 5, 1);
        let short = project(&store, &view);
        assert_eq!(short.rows_layout, GridRows { data: 17, panel: 1 });
        assert_eq!(short.rows.len(), 17);
        for record in 6..=31 {
            goto(&mut store, &mut workspace, &mut view, record, 1);
            let projection = project(&store, &view);
            assert!(
                projection.rows.iter().any(|row| row.record == record),
                "row {record} shows with a panel of {}",
                projection.rows_layout.panel
            );
        }
        let tall = project(&store, &view);
        assert_eq!(
            tall.rows_layout,
            GridRows::of(20),
            "a long cell's panel is the tallest"
        );
        assert!(tall.panel.total_lines > tall.rows_layout.panel);
    }

    /// G2: M-w and ⌘C copy the cell's value, not its quoted bytes, onto the kill ring and the
    /// clipboard, and leave the document alone; a copy never joins the kill before it, and an
    /// empty or missing cell copies nothing.
    #[test]
    fn g2_a_copy_takes_the_value_not_its_quoted_bytes() {
        let (mut store, mut workspace, mut view, document) = grid_editor(MAIN);
        goto(&mut store, &mut workspace, &mut view, 2, 5);
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[EditorInput::Paste("say \"hi\"".into())],
        );
        let pasted = text_of(&store, document);
        assert!(pasted.contains(",\"say \"\"hi\"\"\"\n"), "{pasted}");
        let seq = store.snapshot(document).unwrap().seq;
        for copy in [ch('w', Modifiers::ALT), ch('c', Modifiers::SUPER)] {
            workspace.kill_ring.clear();
            let effects = press(
                &mut store,
                &mut workspace,
                &mut view,
                std::slice::from_ref(&copy),
            );
            assert_eq!(
                workspace.kill_ring.front().map(String::as_str),
                Some("say \"hi\""),
                "{copy:?}"
            );
            assert!(
                effects.contains(&EditorEffect::KillToClipboard {
                    text: "say \"hi\"".into()
                }),
                "{copy:?}: {effects:?}"
            );
            assert_eq!(store.snapshot(document).unwrap().seq, seq, "{copy:?}");
            assert_eq!(cell_of(&store, &mut workspace, &view), (2, 5));
        }
        goto(&mut store, &mut workspace, &mut view, 1, 1);
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[ch('w', Modifiers::ALT)],
        );
        assert_eq!(
            workspace.kill_ring.iter().take(2).collect::<Vec<_>>(),
            ["alice@example.test", "say \"hi\""]
        );
        for (record, column) in [(1, 5), (3, 5)] {
            goto(&mut store, &mut workspace, &mut view, record, column);
            let effects = press(
                &mut store,
                &mut workspace,
                &mut view,
                &[ch('w', Modifiers::ALT)],
            );
            assert_eq!(effects, [EditorEffect::Status(EMPTY_CELL.to_string())]);
            assert_eq!(
                workspace.kill_ring.front().map(String::as_str),
                Some("alice@example.test")
            );
        }
    }

    /// G2: C-w, C-k and ⌘X cut the cell: its value onto the kill ring and the clipboard, the cell
    /// cleared as one undo frame over its own bytes, which undo gives back. An empty cell cuts
    /// nothing.
    #[test]
    fn g2_a_cut_clears_the_cell_in_one_undo_frame() {
        for cut in [
            ch('w', Modifiers::CTRL),
            ch('k', Modifiers::CTRL),
            ch('x', Modifiers::SUPER),
        ] {
            let (mut store, mut workspace, mut view, document) = grid_editor(MIXED);
            goto(&mut store, &mut workspace, &mut view, 2, 2);
            let before = store.snapshot(document).unwrap().seq;
            let effects = press(
                &mut store,
                &mut workspace,
                &mut view,
                std::slice::from_ref(&cut),
            );
            assert_eq!(
                text_of(&store, document),
                MIXED.replace("\"Said \"\"no\"\"\"", "\"\""),
                "{cut:?}"
            );
            assert_eq!(
                store.snapshot(document).unwrap().seq.0,
                before.0 + 1,
                "{cut:?}"
            );
            assert_eq!(
                workspace.kill_ring.front().map(String::as_str),
                Some("Said \"no\""),
                "{cut:?}"
            );
            assert!(
                effects.contains(&EditorEffect::KillToClipboard {
                    text: "Said \"no\"".into()
                }),
                "{cut:?}: {effects:?}"
            );
            assert_eq!(cell_of(&store, &mut workspace, &view), (2, 2));
            press(
                &mut store,
                &mut workspace,
                &mut view,
                &[ch('z', Modifiers::SUPER)],
            );
            assert_eq!(
                text_of(&store, document),
                MIXED,
                "{cut:?}: undo gives it back"
            );
            assert!(!workspace.can_undo(document), "{cut:?}: one undo frame");
        }
        let (mut store, mut workspace, mut view, document) = grid_editor(MIXED);
        goto(&mut store, &mut workspace, &mut view, 1, 3);
        let effects = press(
            &mut store,
            &mut workspace,
            &mut view,
            &[ch('w', Modifiers::CTRL)],
        );
        assert_eq!(effects, [EditorEffect::Status(EMPTY_CELL.to_string())]);
        assert_eq!(text_of(&store, document), MIXED);
        assert!(workspace.kill_ring.is_empty());
    }

    /// G2: C-y puts the newest kill in the cell as one undo frame, quoted as the cell needs and
    /// with one final line break dropped (a killed line's); an empty kill ring says so.
    #[test]
    fn g2_yank_puts_the_newest_kill_in_the_cell() {
        let (mut store, mut workspace, mut view, document) = grid_editor(MIXED);
        let mut probe = view.clone();
        assert_eq!(
            workspace.handle_input(&mut store, &mut probe, ch('y', Modifiers::CTRL)),
            Err(EditorError::NoKill)
        );
        goto(&mut store, &mut workspace, &mut view, 2, 1);
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[ch('w', Modifiers::ALT)],
        );
        goto(&mut store, &mut workspace, &mut view, 1, 3);
        let before = store.snapshot(document).unwrap().seq;
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[ch('y', Modifiers::CTRL)],
        );
        let yanked = MIXED.replace("and two\",\r\n", "and two\",frank@example.test\r\n");
        assert_eq!(text_of(&store, document), yanked);
        assert_eq!(store.snapshot(document).unwrap().seq.0, before.0 + 1);
        assert_eq!(cell_of(&store, &mut workspace, &view), (1, 3));
        workspace.kill_ring.push_front("a, b\n".into());
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[ch('y', Modifiers::CTRL)],
        );
        assert_eq!(
            text_of(&store, document),
            MIXED.replace("and two\",\r\n", "and two\",\"a, b\"\r\n")
        );
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[ch('z', Modifiers::SUPER)],
        );
        assert_eq!(text_of(&store, document), yanked);
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[ch('z', Modifiers::SUPER)],
        );
        assert_eq!(text_of(&store, document), MIXED);
    }

    /// G2: C-o adds a row below with the header's number of empty cells and the line break of the
    /// row above (a break before it when the file had no final newline) and moves to its first
    /// cell; C-S-backspace deletes the row; `grid-insert-row` and `grid-delete-row` (the toolbar's
    /// buttons, M-x) do the same. Each is one undo frame over its own row's bytes, on the CRLF,
    /// mixed and no-final-newline fixtures, and an undo of a delete puts the cursor on the row
    /// put back. *Control (review):* before the cursor fix, undoing Delete Row on a last row with
    /// no line break left the cursor at the end of the row above, (1, 3) on the mixed fixture.
    #[test]
    fn g2_rows_are_added_and_deleted_as_one_undo_frame() {
        let c_o = ch('o', Modifiers::CTRL);
        let kill_row = key(
            Key::Named(NamedKey::Backspace),
            Modifiers::CTRL | Modifiers::SHIFT,
        );
        type Row = (
            &'static str,
            (usize, usize),
            EditorInput,
            String,
            (usize, usize),
        );
        let rows: Vec<Row> = vec![
            (
                MIXED,
                (1, 2),
                c_o.clone(),
                MIXED.replace("and two\",\r\n", "and two\",\r\n,,,\r\n"),
                (2, 0),
            ),
            (
                MIXED,
                (2, 3),
                c_o.clone(),
                format!("{MIXED}\r\n,,,"),
                (3, 0),
            ),
            (
                MIXED,
                (0, 1),
                c_o.clone(),
                MIXED.replacen("fix\r\n", "fix\r\n,,,\r\n", 1),
                (1, 0),
            ),
            (
                ALL_CRLF,
                (2, 0),
                c_o.clone(),
                format!("{ALL_CRLF},,,\r\n"),
                (3, 0),
            ),
            (
                ALL_CRLF,
                (1, 1),
                c_o.clone(),
                ALL_CRLF.replace("line two\",\r\n", "line two\",\r\n,,,\r\n"),
                (2, 0),
            ),
            (
                RACE,
                (3, 2),
                c_o.clone(),
                RACE.replace("Claude\r\n", "Claude\r\n,,,,,\r\n"),
                (4, 0),
            ),
            (
                RACE,
                (4, 5),
                c_o.clone(),
                format!("{RACE}\r\n,,,,,"),
                (5, 0),
            ),
            (
                MIXED,
                (2, 2),
                kill_row.clone(),
                MIXED.replace("\r\nm2,frank@example.test,\"Said \"\"no\"\"\",later", ""),
                (1, 2),
            ),
            (
                MIXED,
                (1, 0),
                kill_row.clone(),
                MIXED.replace("m1,erin@example.test,\"one\nand two\",\r\n", ""),
                (1, 0),
            ),
            (
                ALL_CRLF,
                (1, 3),
                kill_row.clone(),
                ALL_CRLF.replace("m1,gina@example.test,\"line one\r\nline two\",\r\n", ""),
                (1, 3),
            ),
            (
                ALL_CRLF,
                (2, 1),
                kill_row.clone(),
                ALL_CRLF.replace("m2,hal@example.test,plain,\r\n", ""),
                (1, 1),
            ),
            (
                RACE,
                (4, 1),
                kill_row.clone(),
                RACE.replace("\r\nm5,erin@example.test,Claude,SMS,phone on file,", ""),
                (3, 1),
            ),
        ];
        for (fixture, start, input, expected, after) in rows {
            let name = format!("{input:?} at {start:?} of {fixture:?}");
            let (mut store, mut workspace, mut view, document) = grid_editor(fixture);
            goto(&mut store, &mut workspace, &mut view, start.0, start.1);
            let before = store.snapshot(document).unwrap().seq;
            let effects = press(
                &mut store,
                &mut workspace,
                &mut view,
                std::slice::from_ref(&input),
            );
            assert!(effects.is_empty(), "{name}: {effects:?}");
            assert_eq!(text_of(&store, document), expected, "{name}");
            assert_eq!(
                store.snapshot(document).unwrap().seq.0,
                before.0 + 1,
                "{name}: one transaction"
            );
            assert_eq!(cell_of(&store, &mut workspace, &view), after, "{name}");
            press(
                &mut store,
                &mut workspace,
                &mut view,
                &[ch('z', Modifiers::SUPER)],
            );
            assert_eq!(text_of(&store, document), fixture, "{name}: undo");
            assert!(!workspace.can_undo(document), "{name}: one undo frame");
            if input == kill_row {
                // The cell the undo changed: the row put back, a last row with no line break too.
                assert_eq!(
                    cell_of(&store, &mut workspace, &view),
                    (start.0, 0),
                    "{name}: undo puts the cursor on the row put back"
                );
            }
            press(
                &mut store,
                &mut workspace,
                &mut view,
                &[ch('z', Modifiers::SUPER | Modifiers::SHIFT)],
            );
            assert_eq!(text_of(&store, document), expected, "{name}: redo");
            if input == c_o {
                assert_eq!(
                    cell_of(&store, &mut workspace, &view),
                    after,
                    "{name}: redo puts the cursor on the row added again"
                );
            }

            // The command the toolbar button and M-x run does what the key does.
            let command = if input == c_o {
                EditorCommand::GridInsertRow
            } else {
                EditorCommand::GridDeleteRow
            };
            let (mut store, mut workspace, mut view, document) = grid_editor(fixture);
            goto(&mut store, &mut workspace, &mut view, start.0, start.1);
            workspace
                .invoke(&mut store, &mut view, Invocation::new(command))
                .unwrap();
            assert_eq!(text_of(&store, document), expected, "{name}: {command:?}");
            assert_eq!(cell_of(&store, &mut workspace, &view), after, "{name}");
        }
    }

    /// G2: a row command waits while a cell is open, says where it works in a text view, and
    /// leaves alone a row whose quote never closes (it holds every row after it).
    #[test]
    fn g2_rows_wait_for_an_open_cell_and_spare_an_unclosed_row() {
        let (mut store, mut workspace, mut view, document) = grid_editor(MIXED);
        goto(&mut store, &mut workspace, &mut view, 1, 3);
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[EditorInput::Text("done".into())],
        );
        let effects = press(
            &mut store,
            &mut workspace,
            &mut view,
            &[ch('o', Modifiers::CTRL)],
        );
        assert!(
            matches!(effects.as_slice(), [EditorEffect::Status(words)] if words.starts_with("C-o is not a cell key")),
            "{effects:?}"
        );
        for command in [EditorCommand::GridInsertRow, EditorCommand::GridDeleteRow] {
            let effects = workspace.execute(&mut store, &mut view, command).unwrap();
            assert_eq!(effects, [EditorEffect::Status(DRAFT_HOLDS.to_string())]);
        }
        assert_eq!(view.grid_draft().unwrap().text, "done");
        assert_eq!(text_of(&store, document), MIXED);
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[named(NamedKey::Escape), ch('e', Modifiers::SUPER)],
        );
        assert!(view.grid().is_none());
        for command in [EditorCommand::GridInsertRow, EditorCommand::GridDeleteRow] {
            let effects = workspace.execute(&mut store, &mut view, command).unwrap();
            assert!(
                matches!(effects.as_slice(), [EditorEffect::Status(words)] if words.starts_with(&format!("{} works in a table's grid view", command.name()))),
                "{effects:?}"
            );
        }
        assert_eq!(text_of(&store, document), MIXED);

        let (mut store, mut workspace, mut view, document) = grid_editor(STRAY);
        goto(&mut store, &mut workspace, &mut view, 2, 0);
        for input in [
            ch('o', Modifiers::CTRL),
            key(
                Key::Named(NamedKey::Backspace),
                Modifiers::CTRL | Modifiers::SHIFT,
            ),
        ] {
            let effects = press(
                &mut store,
                &mut workspace,
                &mut view,
                std::slice::from_ref(&input),
            );
            assert_eq!(
                effects,
                [EditorEffect::Status(ROW_HOLDS_REST.to_string())],
                "{input:?}"
            );
            assert_eq!(text_of(&store, document), STRAY, "{input:?}");
        }
        goto(&mut store, &mut workspace, &mut view, 0, 0);
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[ch('o', Modifiers::CTRL)],
        );
        assert_eq!(
            text_of(&store, document),
            STRAY.replacen("d\n", "d\n,,\n", 1)
        );

        // A row between a lone CR and a blank LF line: deleting it would drop the blank line too.
        let joined = "a,b\r1,2\n\n3,4\n";
        let (mut store, mut workspace, mut view, document) = grid_editor(joined);
        goto(&mut store, &mut workspace, &mut view, 1, 0);
        let effects = press(
            &mut store,
            &mut workspace,
            &mut view,
            &[key(
                Key::Named(NamedKey::Backspace),
                Modifiers::CTRL | Modifiers::SHIFT,
            )],
        );
        assert_eq!(
            effects,
            [EditorEffect::Status(ROW_JOINS_BREAKS.to_string())]
        );
        assert_eq!(text_of(&store, document), joined);
    }

    /// G2 review: the terminal face never shows a grid, so its row commands (C-S-backspace,
    /// `M-x grid-insert-row`, `grid-delete-row`) point to an aterm Editor tab instead of the C-c
    /// C-t the terminal refuses. *Control:* before this fix they said to press C-c C-t.
    #[test]
    fn g2_row_commands_in_the_terminal_point_to_an_editor_tab() {
        let mut store = DocumentStore::for_test();
        let document = store.open("file:///tmp/t.csv".into(), "a,b\n".into());
        let mut workspace = EditorWorkspace::with_keymap(Keymap::emacs(Profile::Terminal));
        let mut view = workspace
            .attach(&mut store, document, DocumentViewId(6))
            .unwrap();
        for command in [EditorCommand::GridInsertRow, EditorCommand::GridDeleteRow] {
            let effects = workspace.execute(&mut store, &mut view, command).unwrap();
            assert_eq!(
                effects,
                vec![EditorEffect::Status(TERMINAL_TABLE.to_string())],
                "{command:?}"
            );
        }
        let effects = workspace
            .handle_input(
                &mut store,
                &mut view,
                key(
                    Key::Named(NamedKey::Backspace),
                    Modifiers::CTRL | Modifiers::SHIFT,
                ),
            )
            .unwrap()
            .effects;
        assert_eq!(
            effects,
            vec![EditorEffect::Status(TERMINAL_TABLE.to_string())]
        );
        assert_eq!(text_of(&store, document), "a,b\n");
    }

    /// G2 review: a value the grid copied or cut comes back whole on C-y or a paste, its final
    /// line break included: C-w then C-y in place leaves the bytes as they were for a value
    /// ending in LF or CRLF, for one ending in two breaks and for a lone break, and M-w then C-y
    /// (or a paste of that same text) in another cell writes the value copied. A kill from the
    /// text view still drops one final break. *Control:* the yank before this fix put every kill
    /// through `cell_text`, so `"x\n"` came back `"x"` and a lone-break cell stayed `""`.
    #[test]
    fn g2_a_grid_value_yanks_back_whole() {
        let cut = ch('w', Modifiers::CTRL);
        let yank = ch('y', Modifiers::CTRL);
        for cell in ["\"x\n\"", "\"\n\"", "\"line\r\n\"", "\"a\nb\n\n\""] {
            let text = format!("h1,h2\r\n{cell},y\r\n");
            let (mut store, mut workspace, mut view, document) = grid_editor(&text);
            goto(&mut store, &mut workspace, &mut view, 1, 0);
            press(
                &mut store,
                &mut workspace,
                &mut view,
                std::slice::from_ref(&cut),
            );
            assert_eq!(
                text_of(&store, document),
                "h1,h2\r\n\"\",y\r\n",
                "{cell}: cleared, its quotes kept"
            );
            press(
                &mut store,
                &mut workspace,
                &mut view,
                std::slice::from_ref(&yank),
            );
            assert_eq!(
                text_of(&store, document),
                text,
                "{cell}: cut and yank in place"
            );
        }
        let text = "h1,h2\n\"a\nb\n\",\n";
        let copied = "h1,h2\n\"a\nb\n\",\"a\nb\n\"\n";
        let (mut store, mut workspace, mut view, document) = grid_editor(text);
        goto(&mut store, &mut workspace, &mut view, 1, 0);
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[ch('w', Modifiers::ALT)],
        );
        goto(&mut store, &mut workspace, &mut view, 1, 1);
        press(
            &mut store,
            &mut workspace,
            &mut view,
            std::slice::from_ref(&yank),
        );
        assert_eq!(text_of(&store, document), copied, "M-w then C-y");
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[
                ch('z', Modifiers::SUPER),
                EditorInput::Paste("a\nb\n".into()),
            ],
        );
        assert_eq!(text_of(&store, document), copied, "M-w then a paste");
        // A text kill is not a cell's value: its final break goes.
        workspace.record_kill("z\n".to_string(), None, false);
        press(
            &mut store,
            &mut workspace,
            &mut view,
            std::slice::from_ref(&yank),
        );
        assert_eq!(
            text_of(&store, document),
            "h1,h2\n\"a\nb\n\",\"z\"\n",
            "the quoted cell keeps its quotes; the kill's break goes"
        );
    }

    /// G2 review: a document held with LF for CRLF on disk (`crlf_on_disk`, which the GUI sets)
    /// takes a yanked kill's CRLF as LF, in the grid and in the text view, so its file does not
    /// save CR CR LF. *Control:* with `crlf_on_disk` off, the same yank writes the CRLF, as every
    /// yank did before this fix.
    #[test]
    fn g2_a_yank_into_a_crlf_held_document_writes_lf() {
        let text = "h1,h2\na,\n";
        for held in [true, false] {
            let (mut store, mut workspace, mut view, document) = grid_editor(text);
            workspace.crlf_on_disk = held;
            workspace.kill_ring.push_front("x\r\ny".into());
            goto(&mut store, &mut workspace, &mut view, 1, 1);
            press(
                &mut store,
                &mut workspace,
                &mut view,
                &[ch('y', Modifiers::CTRL)],
            );
            let cell = if held { "\"x\ny\"" } else { "\"x\r\ny\"" };
            assert_eq!(
                text_of(&store, document),
                format!("h1,h2\na,{cell}\n"),
                "held={held}"
            );

            press(
                &mut store,
                &mut workspace,
                &mut view,
                &[ch('z', Modifiers::SUPER), ch('e', Modifiers::SUPER)],
            );
            assert!(view.grid().is_none());
            press(
                &mut store,
                &mut workspace,
                &mut view,
                &[ch('y', Modifiers::CTRL)],
            );
            let after = text_of(&store, document);
            assert_eq!(after.contains('\r'), !held, "held={held}: {after:?}");
            assert!(
                after.contains(if held { "x\ny" } else { "x\r\ny" }),
                "{after:?}"
            );
        }
    }

    /// G2: a press in the cell panel puts the cell's caret at the boundary nearest it, opening
    /// the cell first when it was closed; nothing lands before Return, and a press inside a wide
    /// character lands on a boundary.
    #[test]
    fn g2_a_press_in_the_panel_places_the_caret() {
        let (mut store, mut workspace, mut view, document) = grid_editor(MAIN);
        let project = |store: &DocumentStore, view: &EditorBufferView| {
            let snapshot = store.snapshot(view.document).unwrap();
            let table = Table::scan(&snapshot.text, Delimiter::Comma).unwrap();
            project_grid(&snapshot.text, &table, view, 20, 60)
        };
        goto(&mut store, &mut workspace, &mut view, 2, 4);
        let value = "Two reasons: first\nthe phone prompt, then the code";
        let grid = project(&store, &view);
        assert_eq!(grid.panel_value(), value);
        assert_eq!(grid.panel.ranges, [0..18, 19..value.len()]);
        assert_eq!(
            grid.panel_byte(1, 8),
            Some(23),
            "the second line, four cells (eight half cells) in"
        );
        assert_eq!(
            grid.panel_byte(1, 9),
            Some(24),
            "the second half of a cell: after it"
        );
        assert_eq!(
            grid.panel_byte(0, 800),
            Some(18),
            "past a line's end, its end"
        );
        assert_eq!(grid.panel_byte(2, 0), None, "past the shown lines");
        let seq = store.snapshot(document).unwrap().seq;
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[EditorInput::GridPanelPointer { byte: 23 }],
        );
        let draft = view.grid_draft().expect("the press opens the cell");
        assert_eq!((draft.text.as_str(), draft.caret), (value, 23));
        assert_eq!(project(&store, &view).panel_byte(0, 6), Some(3));
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[
                EditorInput::GridPanelPointer { byte: 3 },
                ch('!', Modifiers::empty()),
            ],
        );
        assert_eq!(
            view.grid_draft().unwrap().text,
            "Two! reasons: first\nthe phone prompt, then the code"
        );
        assert_eq!(
            store.snapshot(document).unwrap().seq,
            seq,
            "nothing lands yet"
        );
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[named(NamedKey::Escape)],
        );
        goto(&mut store, &mut workspace, &mut view, 1, 5);
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[EditorInput::Text("日本語".into()), named(NamedKey::Enter)],
        );
        goto(&mut store, &mut workspace, &mut view, 1, 5);
        let grid = project(&store, &view);
        assert_eq!(grid.panel_value(), "日本語");
        assert_eq!(grid.panel_byte(0, 0), Some(0));
        assert_eq!(
            grid.panel_byte(0, 1),
            Some(0),
            "the second quarter of a wide character: before it"
        );
        assert_eq!(grid.panel_byte(0, 2), Some(3), "its far half: after it");
        assert_eq!(grid.panel_byte(0, 5), Some(3));
        assert_eq!(grid.panel_byte(0, 6), Some(6));
        press(
            &mut store,
            &mut workspace,
            &mut view,
            &[EditorInput::GridPanelPointer { byte: 4 }],
        );
        assert_eq!(
            view.grid_draft().unwrap().caret,
            3,
            "inside a character: its start"
        );
    }
}
