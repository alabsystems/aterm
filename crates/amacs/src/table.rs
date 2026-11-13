// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CSV and TSV as records and cells over the document's own bytes (design
//! `DESIGN-amacs-grid-2026-10-28.md` §4): a tolerant scan that never rewrites,
//! a cell's value, the minimal re-encoding a commit writes, and the widths,
//! clipping and wrapping both faces lay a grid out with.

use std::borrow::Cow;
use std::ops::Range;
use std::sync::{Arc, OnceLock};

use aterm_buffer::Seq;
use aterm_grapheme::{GraphemeClusters, grapheme_display_width};

use crate::store::{DocumentId, TextEdit};

/// The most bytes the grid scans (G6 raises it toward the document limit).
pub const MAX_TABLE_BYTES: usize = 4 * 1024 * 1024;
/// The most records the grid scans.
pub const MAX_TABLE_RECORDS: usize = 100_000;
/// The most cells one record may hold.
pub const MAX_TABLE_COLUMNS: usize = 1024;
/// The narrowest a column is drawn, in character cells.
pub const MIN_COLUMN_WIDTH: usize = 3;
/// The widest a column is drawn, in character cells; the panel shows the rest.
pub const MAX_COLUMN_WIDTH: usize = 40;

/// What separates the cells of a record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Delimiter {
    /// `,`: a `.csv` file.
    Comma,
    /// A tab: a `.tsv` file.
    Tab,
}

impl Delimiter {
    /// The delimiter's byte.
    #[inline]
    #[must_use]
    pub const fn byte(self) -> u8 {
        match self {
            Self::Comma => b',',
            Self::Tab => b'\t',
        }
    }

    /// The delimiter's character.
    #[inline]
    #[must_use]
    pub const fn char(self) -> char {
        match self {
            Self::Comma => ',',
            Self::Tab => '\t',
        }
    }
}

/// Why a text is not scanned as a grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TooLarge {
    /// The text is longer than [`MAX_TABLE_BYTES`].
    Bytes(usize),
    /// The text holds more than [`MAX_TABLE_RECORDS`] records.
    Records,
    /// A record holds more than [`MAX_TABLE_COLUMNS`] cells.
    Columns,
}

impl TooLarge {
    /// What a status line says when a table opens as text instead.
    #[must_use]
    pub fn reason(self) -> String {
        match self {
            Self::Bytes(len) => format!(
                "Opened as text: the grid reads files up to {} MiB, and this one is {:.1} MiB",
                MAX_TABLE_BYTES / (1024 * 1024),
                len as f64 / (1024.0 * 1024.0)
            ),
            Self::Records => format!(
                "Opened as text: the grid reads up to {MAX_TABLE_RECORDS} rows, and this file has more"
            ),
            Self::Columns => format!(
                "Opened as text: the grid reads up to {MAX_TABLE_COLUMNS} columns, and a row of this file has more"
            ),
        }
    }
}

/// One cell: where it sits in the text, quotes included.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    /// Every byte of the cell: its quotes and any text after its closing quote.
    pub raw: Range<usize>,
    /// The bytes inside the quotes, `""` still doubled; `raw` for a bare cell.
    pub content: Range<usize>,
    /// Text after the closing quote, before the delimiter: a problem kept as it is (empty for most cells).
    pub after: Range<usize>,
    /// Whether the cell starts with a quote.
    pub quoted: bool,
}

impl Cell {
    /// The cell's value: `""` inside quotes reads as `"`, and text after the closing quote is kept.
    #[must_use]
    pub fn value<'a>(&self, text: &'a str) -> Cow<'a, str> {
        let content = text.get(self.content.clone()).unwrap_or_default();
        if !self.quoted {
            return Cow::Borrowed(content);
        }
        let after = text.get(self.after.clone()).unwrap_or_default();
        if !content.contains("\"\"") && after.is_empty() {
            return Cow::Borrowed(content);
        }
        let mut value = content.replace("\"\"", "\"");
        value.push_str(after);
        Cow::Owned(value)
    }

    /// Whether `at` is in this cell, its last byte's end included: a caret on
    /// the delimiter after a cell is still that cell's.
    #[inline]
    #[must_use]
    pub const fn holds(&self, at: usize) -> bool {
        self.raw.start <= at && at <= self.raw.end
    }
}

/// One record: its cells and the line break that ends it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    /// The record's bytes before its terminator.
    pub raw: Range<usize>,
    /// The record's cells, in order; at least one.
    pub cells: Vec<Cell>,
    /// `\n`, `\r\n` or `\r`; empty for a last record with no final line break.
    pub terminator: Range<usize>,
}

/// What the scan noticed and kept as it was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProblemKind {
    /// A quote that never closes: the cell runs to the end of the text.
    UnclosedQuote,
    /// Text after a closing quote, before the delimiter.
    TextAfterQuote,
}

/// One problem and the byte it starts at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Problem {
    pub at: usize,
    pub kind: ProblemKind,
}

/// A text read as records and cells. It holds ranges, never a copy of the text.
#[derive(Clone, Debug)]
pub struct Table {
    pub delimiter: Delimiter,
    pub records: Vec<Record>,
    pub problems: Vec<Problem>,
    columns: usize,
    widths: OnceLock<Vec<usize>>,
}

impl Table {
    /// Read `text` as records of cells split by `delimiter`.
    ///
    /// The scan is tolerant and never refuses well-formed or broken text: a
    /// quote inside a bare cell is plain text, a quote that never closes runs
    /// to the end, and text after a closing quote is kept; both are noted in
    /// [`Self::problems`]. Outside quotes `\r\n`, `\n` and `\r` each end a
    /// record; inside quotes they belong to the cell. A final line break does
    /// not start an empty record, and an empty text is one empty record.
    ///
    /// # Errors
    ///
    /// [`TooLarge`] when the text passes [`MAX_TABLE_BYTES`], or holds more
    /// than [`MAX_TABLE_RECORDS`] records or a record of more than
    /// [`MAX_TABLE_COLUMNS`] cells.
    pub fn scan(text: &str, delimiter: Delimiter) -> Result<Self, TooLarge> {
        if text.len() > MAX_TABLE_BYTES {
            return Err(TooLarge::Bytes(text.len()));
        }
        let bytes = text.as_bytes();
        let len = bytes.len();
        let delim = delimiter.byte();
        let ends_bare = |byte: u8| byte == delim || byte == b'\n' || byte == b'\r';
        let mut records = Vec::new();
        let mut problems = Vec::new();
        let mut columns = 0;
        let mut pos = 0;
        loop {
            let record_start = pos;
            let mut cells = Vec::new();
            loop {
                if cells.len() >= MAX_TABLE_COLUMNS {
                    return Err(TooLarge::Columns);
                }
                let start = pos;
                if bytes.get(pos) == Some(&b'"') {
                    let mut at = pos.saturating_add(1);
                    let close = loop {
                        match bytes.get(at..).and_then(|rest| {
                            rest.iter()
                                .position(|&byte| byte == b'"')
                                .map(|offset| at.saturating_add(offset))
                        }) {
                            None => break None,
                            Some(quote) if bytes.get(quote.saturating_add(1)) == Some(&b'"') => {
                                at = quote.saturating_add(2);
                            }
                            Some(quote) => break Some(quote),
                        }
                    };
                    let Some(close) = close else {
                        problems.push(Problem {
                            at: start,
                            kind: ProblemKind::UnclosedQuote,
                        });
                        cells.push(Cell {
                            raw: start..len,
                            content: start.saturating_add(1)..len,
                            after: len..len,
                            quoted: true,
                        });
                        pos = len;
                        break;
                    };
                    let after_start = close.saturating_add(1);
                    let mut end = after_start;
                    while bytes.get(end).is_some_and(|&byte| !ends_bare(byte)) {
                        end = end.saturating_add(1);
                    }
                    if end > after_start {
                        problems.push(Problem {
                            at: after_start,
                            kind: ProblemKind::TextAfterQuote,
                        });
                    }
                    cells.push(Cell {
                        raw: start..end,
                        content: start.saturating_add(1)..close,
                        after: after_start..end,
                        quoted: true,
                    });
                    pos = end;
                } else {
                    let mut end = pos;
                    while bytes.get(end).is_some_and(|&byte| !ends_bare(byte)) {
                        end = end.saturating_add(1);
                    }
                    cells.push(Cell {
                        raw: start..end,
                        content: start..end,
                        after: end..end,
                        quoted: false,
                    });
                    pos = end;
                }
                if bytes.get(pos) == Some(&delim) {
                    pos = pos.saturating_add(1);
                    continue;
                }
                break;
            }
            let raw_end = pos;
            let terminator_len = match (bytes.get(pos), bytes.get(pos.saturating_add(1))) {
                (Some(b'\r'), Some(b'\n')) => 2,
                (Some(b'\r' | b'\n'), _) => 1,
                _ => 0,
            };
            pos = pos.saturating_add(terminator_len);
            columns = columns.max(cells.len());
            records.push(Record {
                raw: record_start..raw_end,
                cells,
                terminator: raw_end..pos,
            });
            if records.len() > MAX_TABLE_RECORDS {
                return Err(TooLarge::Records);
            }
            if pos >= len {
                break;
            }
        }
        Ok(Self {
            delimiter,
            records,
            problems,
            columns,
            widths: OnceLock::new(),
        })
    }

    /// The most cells any record holds.
    #[inline]
    #[must_use]
    pub const fn columns(&self) -> usize {
        self.columns
    }

    /// The record that holds byte `at`, its terminator included; the last record past the end.
    #[must_use]
    pub fn record_at(&self, at: usize) -> usize {
        self.records
            .partition_point(|record| record.terminator.end <= at && !record.terminator.is_empty())
            .min(self.records.len().saturating_sub(1))
    }

    /// The cell of `record` that holds byte `at` ([`Cell::holds`]); the last cell when none does.
    #[must_use]
    pub fn cell_at(&self, record: usize, at: usize) -> usize {
        let Some(record) = self.records.get(record) else {
            return 0;
        };
        record
            .cells
            .iter()
            .position(|cell| cell.holds(at))
            .unwrap_or(record.cells.len().saturating_sub(1))
    }

    /// The cell at `record`, `column`, when the record holds that many cells.
    #[must_use]
    pub fn cell(&self, record: usize, column: usize) -> Option<&Cell> {
        self.records.get(record)?.cells.get(column)
    }

    /// Where the caret goes for a cell: inside its opening quote, or at its
    /// record's end for a cell the record lacks.
    #[must_use]
    pub fn caret_for(&self, record: usize, column: usize) -> usize {
        let Some(row) = self.records.get(record) else {
            return 0;
        };
        match row.cells.get(column) {
            Some(cell) if cell.quoted => cell.content.start.min(cell.raw.end),
            Some(cell) => cell.raw.start,
            None => row.raw.end,
        }
    }

    /// Whether `record` ends in a quote that never closes, which would swallow a cell added after it.
    #[must_use]
    pub fn unclosed(&self, record: usize) -> bool {
        self.records
            .get(record)
            .and_then(|row| row.cells.last())
            .is_some_and(|cell| self.opens_unclosed(cell))
    }

    /// Whether the cell at `record`, `column` opens a quote that never closes: it runs to the end
    /// of the text, every later row inside it.
    #[must_use]
    pub fn cell_unclosed(&self, record: usize, column: usize) -> bool {
        self.cell(record, column)
            .is_some_and(|cell| self.opens_unclosed(cell))
    }

    fn opens_unclosed(&self, cell: &Cell) -> bool {
        self.problems.iter().any(|problem| {
            problem.kind == ProblemKind::UnclosedQuote && problem.at == cell.raw.start
        })
    }

    /// The first problem, as the 1-based line it starts on.
    #[must_use]
    pub fn first_problem_line(&self, text: &str) -> Option<usize> {
        let problem = self.problems.first()?;
        Some(line_of(text, problem.at))
    }

    /// Each column's drawn width: its widest cell's first line, header
    /// included, from [`MIN_COLUMN_WIDTH`] to [`MAX_COLUMN_WIDTH`] cells.
    /// Computed once per table.
    pub fn column_widths(&self, text: &str) -> &[usize] {
        self.widths.get_or_init(|| {
            let mut widths = vec![MIN_COLUMN_WIDTH; self.columns];
            for record in &self.records {
                for (column, cell) in record.cells.iter().enumerate() {
                    let Some(width) = widths.get_mut(column) else {
                        continue;
                    };
                    if *width >= MAX_COLUMN_WIDTH {
                        continue;
                    }
                    let value = cell.value(text);
                    let first = first_line(&value);
                    let marker = usize::from(first.len() < value.len());
                    let cells = text_width_capped(first, MAX_COLUMN_WIDTH).saturating_add(marker);
                    *width = (*width).max(cells.min(MAX_COLUMN_WIDTH));
                }
            }
            widths
        })
    }
}

/// The 1-based line byte `at` of `text` is on.
#[must_use]
pub fn line_of(text: &str, at: usize) -> usize {
    let at = at.min(text.len());
    text.as_bytes()
        .get(..at)
        .map_or(0, |head| head.iter().filter(|&&byte| byte == b'\n').count())
        .saturating_add(1)
}

/// `value` written for a cell: quoted when `keep_quotes` (a cell that had quotes keeps them) or
/// when the value holds the delimiter, `"`, CR or LF; a `"` inside quotes is written `""`.
#[must_use]
pub fn encode(value: &str, delimiter: Delimiter, keep_quotes: bool) -> Cow<'_, str> {
    let needs = value
        .bytes()
        .any(|byte| byte == delimiter.byte() || matches!(byte, b'"' | b'\r' | b'\n'));
    if !(keep_quotes || needs) {
        return Cow::Borrowed(value);
    }
    let mut out = String::with_capacity(value.len().saturating_add(2));
    out.push('"');
    for character in value.chars() {
        if character == '"' {
            out.push('"');
        }
        out.push(character);
    }
    out.push('"');
    Cow::Owned(out)
}

/// Why [`edit_cell`] writes nothing where a value was asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unwritable {
    /// The cell opens a quote that never closes, so it runs to the end of the text: writing it
    /// would replace every row after it.
    UnclosedCell,
    /// The record lacks the cell and ends in a quote that never closes, which would swallow a
    /// cell added after it.
    UnclosedRow,
    /// Deleting the record would bring the lone `\r` ending the record above it next to the `\n`
    /// of a blank line below it, which a reader takes as one `\r\n`: a second row would go.
    JoinsLineBreaks,
}

/// The one edit that gives the cell at `record`, `column` the value `value`, or `Ok(None)` when it
/// already has it (an empty value for a cell the record lacks included). The edit covers only the
/// cell's own bytes; a missing cell is one insertion at its record's end, with the delimiters it
/// needs. A record's only cell cleared is written `""`, so the record stays a row rather than a
/// blank line readers skip or a final line break that ends the text.
///
/// # Errors
///
/// [`Unwritable::UnclosedCell`] for a cell whose quote never closes ([`Table::cell_unclosed`]),
/// and [`Unwritable::UnclosedRow`] for a cell the record lacks after one ([`Table::unclosed`]),
/// unless the value asked for is the one the cell already reads as.
pub fn edit_cell(
    table: &Table,
    text: &str,
    record: usize,
    column: usize,
    value: &str,
) -> Result<Option<TextEdit>, Unwritable> {
    let Some(row) = table.records.get(record) else {
        return Ok(None);
    };
    if let Some(cell) = row.cells.get(column) {
        if cell.value(text) == value {
            return Ok(None);
        }
        if table.opens_unclosed(cell) {
            return Err(Unwritable::UnclosedCell);
        }
        let only = row.cells.len() == 1 && value.is_empty();
        return Ok(Some(TextEdit {
            range: cell.raw.clone(),
            insert: encode(value, table.delimiter, cell.quoted || only).into_owned(),
        }));
    }
    if value.is_empty() || column >= MAX_TABLE_COLUMNS {
        return Ok(None);
    }
    if table.unclosed(record) {
        return Err(Unwritable::UnclosedRow);
    }
    let pad = column.saturating_sub(row.cells.len()).saturating_add(1);
    let mut insert = String::new();
    for _ in 0..pad {
        insert.push(table.delimiter.char());
    }
    insert.push_str(&encode(value, table.delimiter, false));
    Ok(Some(TextEdit {
        range: row.raw.end..row.raw.end,
        insert,
    }))
}

/// The one edit that adds an empty row after `record`, or `Ok(None)` past the last record. The
/// row holds the header's number of empty cells (a one-column table's is written `""`, so the
/// row is not a blank line readers skip) and ends with `record`'s own line break. After a last
/// record with no line break, the row is preceded by one instead (the nearest earlier record's,
/// else `\n`), and the text still ends without one. The edit is one insertion between `record`
/// and the record after it: no other byte moves.
///
/// # Errors
///
/// [`Unwritable::UnclosedRow`] when `record` ends in a quote that never closes: a row added after
/// it would land inside that cell.
pub fn insert_row(
    table: &Table,
    text: &str,
    record: usize,
) -> Result<Option<TextEdit>, Unwritable> {
    let Some(row) = table.records.get(record) else {
        return Ok(None);
    };
    if table.unclosed(record) {
        return Err(Unwritable::UnclosedRow);
    }
    let cells = table
        .records
        .first()
        .map_or(1, |header| header.cells.len())
        .max(1);
    let empty = if cells == 1 {
        "\"\"".to_string()
    } else {
        table
            .delimiter
            .char()
            .to_string()
            .repeat(cells.saturating_sub(1))
    };
    let own = text.get(row.terminator.clone()).unwrap_or_default();
    if !own.is_empty() {
        return Ok(Some(TextEdit {
            range: row.terminator.end..row.terminator.end,
            insert: format!("{empty}{own}"),
        }));
    }
    let line_break = table
        .records
        .get(..record)
        .unwrap_or_default()
        .iter()
        .rev()
        .find_map(|earlier| {
            text.get(earlier.terminator.clone())
                .filter(|terminator| !terminator.is_empty())
        })
        .unwrap_or("\n");
    Ok(Some(TextEdit {
        range: row.raw.end..row.raw.end,
        insert: format!("{line_break}{empty}"),
    }))
}

/// The one edit that deletes `record`, or `Ok(None)` when there is nothing to delete: the
/// record's bytes and its line break. A last record with no line break takes the break before it
/// instead, so the text still ends as it did, unless the record above is a blank line: a blank
/// line ends the text only with its break, so then the record's bytes alone go and the text ends
/// with that blank line's break.
///
/// # Errors
///
/// [`Unwritable::UnclosedRow`] when `record` ends in a quote that never closes: that cell holds
/// every row after it, which the deletion would take along. [`Unwritable::JoinsLineBreaks`] when
/// the record above ends in a lone `\r` and the record below is a blank `\n` line: the two would
/// read as one `\r\n`, dropping the blank line too.
pub fn delete_row(
    table: &Table,
    text: &str,
    record: usize,
) -> Result<Option<TextEdit>, Unwritable> {
    let Some(row) = table.records.get(record) else {
        return Ok(None);
    };
    if table.unclosed(record) {
        return Err(Unwritable::UnclosedRow);
    }
    let range = match record.checked_sub(1).and_then(|at| table.records.get(at)) {
        Some(previous) if row.terminator.is_empty() && previous.raw.is_empty() => row.raw.clone(),
        Some(previous) if row.terminator.is_empty() => previous.terminator.start..row.raw.end,
        _ => row.raw.start..row.terminator.end,
    };
    let joins = text
        .get(..range.start)
        .is_some_and(|before| before.ends_with('\r'))
        && text
            .get(range.end..)
            .is_some_and(|after| after.starts_with('\n'));
    if joins {
        return Err(Unwritable::JoinsLineBreaks);
    }
    Ok((!range.is_empty()).then(|| TextEdit {
        range,
        insert: String::new(),
    }))
}

/// `value` up to its first line break.
#[must_use]
pub fn first_line(value: &str) -> &str {
    value
        .find(['\r', '\n'])
        .map_or(value, |end| value.get(..end).unwrap_or(value))
}

/// The cells one grapheme of a cell takes: a tab and other control characters take one (drawn as a space or `·`).
pub(crate) fn grapheme_cells(grapheme: &str) -> usize {
    if grapheme.chars().any(char::is_control) {
        1
    } else {
        grapheme_display_width(grapheme)
    }
}

/// The cells `text` takes, counted no further than `cap`.
fn text_width_capped(text: &str, cap: usize) -> usize {
    let mut width = 0usize;
    for grapheme in text.graphemes() {
        width = width.saturating_add(grapheme_cells(grapheme));
        if width > cap {
            break;
        }
    }
    width
}

/// The cells `text` takes.
#[must_use]
pub fn text_width(text: &str) -> usize {
    text_width_capped(text, usize::MAX)
}

/// `text` as a face draws it: a tab as a space and any other control character as `·`.
#[must_use]
pub fn display(text: &str) -> Cow<'_, str> {
    if !text.chars().any(char::is_control) {
        return Cow::Borrowed(text);
    }
    Cow::Owned(
        text.chars()
            .map(|character| match character {
                '\t' => ' ',
                character if character.is_control() => '·',
                character => character,
            })
            .collect(),
    )
}

/// A cell's first line fitted to `width` cells: cut with `…` when it is wider, and ended with
/// `⏎` when the value goes on past a line break. Grapheme-safe: a cluster is never split.
#[must_use]
pub fn clip(value: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let first = first_line(value);
    let more = first.len() < value.len();
    let whole = text_width_capped(first, width.saturating_add(1)).saturating_add(usize::from(more));
    if whole <= width {
        let mut out = display(first).into_owned();
        if more {
            out.push('⏎');
        }
        return out;
    }
    let room = width.saturating_sub(1);
    let mut used = 0usize;
    let mut end = 0usize;
    for (at, grapheme) in first.grapheme_indices() {
        let cells = grapheme_cells(grapheme);
        if used.saturating_add(cells) > room {
            break;
        }
        used = used.saturating_add(cells);
        end = at.saturating_add(grapheme.len());
    }
    let mut out = display(first.get(..end).unwrap_or_default()).into_owned();
    out.push('…');
    out
}

/// `value` laid out on lines `width` cells wide, as byte ranges of `value`: each line break
/// starts a line (the break itself is in no range), and a line too wide breaks after its last
/// space, or at a grapheme boundary when it has none. An empty value is one empty line.
#[must_use]
pub fn wrap(value: &str, width: usize) -> Vec<Range<usize>> {
    let width = width.max(1);
    let mut out = Vec::new();
    let bytes = value.as_bytes();
    let mut line_start = 0usize;
    loop {
        let line_end = value
            .get(line_start..)
            .and_then(|rest| rest.find(['\r', '\n']))
            .map_or(value.len(), |offset| line_start.saturating_add(offset));
        let line = value.get(line_start..line_end).unwrap_or_default();
        let mut start = line_start;
        let mut used = 0usize;
        let mut last_space_end: Option<usize> = None;
        for (offset, grapheme) in line.grapheme_indices() {
            let at = line_start.saturating_add(offset);
            let cells = grapheme_cells(grapheme);
            if grapheme == " " && used.saturating_add(cells) > width {
                // A space hangs past the line's end: it ends this line rather than starting the next.
                out.push(start..at.saturating_add(1));
                start = at.saturating_add(1);
                used = 0;
                last_space_end = None;
                continue;
            }
            if used.saturating_add(cells) > width && at > start {
                let cut = last_space_end.filter(|&end| end > start).unwrap_or(at);
                out.push(start..cut);
                start = cut;
                used = text_width(value.get(start..at).unwrap_or_default());
                last_space_end = None;
            }
            used = used.saturating_add(cells);
            if grapheme == " " {
                last_space_end = Some(at.saturating_add(1));
            }
        }
        out.push(start..line_end);
        if line_end >= value.len() {
            break;
        }
        let break_len = match (bytes.get(line_end), bytes.get(line_end.saturating_add(1))) {
            (Some(b'\r'), Some(b'\n')) => 2,
            _ => 1,
        };
        line_start = line_end.saturating_add(break_len);
        if line_start > value.len() {
            break;
        }
    }
    out
}

/// The line of `lines` (from [`wrap`]) that byte `caret` of the wrapped value is on, and its
/// cell column there.
#[must_use]
pub fn wrapped_caret(value: &str, lines: &[Range<usize>], caret: usize) -> (usize, usize) {
    let line = lines
        .iter()
        .rposition(|line| line.start <= caret)
        .unwrap_or(0);
    let start = lines.get(line).map_or(0, |line| line.start);
    let column = text_width(value.get(start..caret.max(start)).unwrap_or_default());
    (line, column)
}

/// The scan of one document revision, kept until the revision changes (the pattern of the
/// editor's line-index memo).
#[derive(Debug, Default)]
pub struct TableMemo {
    last: Option<KeptScan>,
}

/// A [`TableMemo`]'s kept scan: the document, its revision and the delimiter it was split by.
type KeptScan = (DocumentId, Seq, Delimiter, Result<Arc<Table>, TooLarge>);

impl TableMemo {
    /// The scan of `text`, the text of `document` at `seq`, by `delimiter`: the kept one when
    /// nothing changed, else a new scan.
    ///
    /// # Errors
    ///
    /// The scan's [`TooLarge`], kept like a table.
    pub fn table(
        &mut self,
        document: DocumentId,
        seq: Seq,
        delimiter: Delimiter,
        text: &str,
    ) -> Result<Arc<Table>, TooLarge> {
        if let Some((kept_document, kept_seq, kept_delimiter, kept)) = &self.last
            && *kept_document == document
            && *kept_seq == seq
            && *kept_delimiter == delimiter
        {
            return kept.clone();
        }
        let scanned = Table::scan(text, delimiter).map(Arc::new);
        self.last = Some((document, seq, delimiter, scanned.clone()));
        scanned
    }
}

#[cfg(test)]
#[path = "table_fixtures_tests.rs"]
pub(crate) mod fixtures;

#[cfg(test)]
#[path = "table_tests.rs"]
mod tests;
