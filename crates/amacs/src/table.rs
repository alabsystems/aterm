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
pub(crate) mod fixtures {
    //! Synthetic tables of the owner's file's shape: `@example.test` addresses, a long-text
    //! column, quoted commas, `""`, a line break inside a quoted cell, ragged rows and empty
    //! cells, every line-ending mix, no final newline, a stray quote, and a TSV twin. No test
    //! reads a real file.

    /// The header and four rows, LF line ends: a short row (m3) and a long one (m4).
    pub(crate) const MAIN: &str = concat!(
        "computer,AI login email,services,codes arrive in (my guess),why I guessed,your correction (leave blank if my guess is right)\n",
        "m1,alice@example.test,Claude,email,\"The sign-in page said \"\"check your inbox\"\", so email.\",\n",
        "m2,bob@example.test,\"Claude, GitHub\",SMS,\"Two reasons: first\nthe phone prompt, then the code\",\n",
        "m3,carol@example.test,Claude,authenticator\n",
        "m4,dave@example.test,,,,,extra\n",
    );

    /// CRLF between records and LF inside a quoted cell, as Excel writes; no final newline.
    pub(crate) const MIXED: &str = concat!(
        "computer,email,why,fix\r\n",
        "m1,erin@example.test,\"one\nand two\",\r\n",
        "m2,frank@example.test,\"Said \"\"no\"\"\",later",
    );

    /// Every break CRLF, inside the quoted cell too.
    pub(crate) const ALL_CRLF: &str = concat!(
        "computer,email,why,fix\r\n",
        "m1,gina@example.test,\"line one\r\nline two\",\r\n",
        "m2,hal@example.test,plain,\r\n",
    );

    /// A quote inside a bare cell, text after a closing quote, and a quote that never closes.
    pub(crate) const STRAY: &str =
        concat!("a,b\"c,d\n", "\"x\"y,z\n", "\"never closes,\nstill open",);

    /// [`MAIN`] with tabs: commas and quotes in values need no quotes, a tab or line break does.
    pub(crate) const TSV: &str = concat!(
        "computer\tAI login email\tservices\twhy I guessed\tyour correction\n",
        "m1\talice@example.test\tClaude, GitHub\tsaid \"inbox\", so email\t\n",
        "m2\tbob@example.test\tClaude\t\"Two\tparts\"\t\n",
    );
}

#[cfg(test)]
mod tests {
    use super::fixtures::{ALL_CRLF, MAIN, MIXED, STRAY, TSV};
    use super::*;

    /// One fixture's expected reading: each record's cells as raw text, and its terminator.
    type Expected<'a> = &'a [(&'a [&'a str], &'a str)];

    /// Whether `scan` reads `text` as `expected`, or what it read instead.
    fn check_scan(
        scan: impl Fn(&str, Delimiter) -> Result<Table, TooLarge>,
        text: &str,
        delimiter: Delimiter,
        expected: Expected<'_>,
    ) -> Result<(), String> {
        let table = scan(text, delimiter).map_err(|error| format!("{error:?}"))?;
        let read = table
            .records
            .iter()
            .map(|record| {
                (
                    record
                        .cells
                        .iter()
                        .map(|cell| &text[cell.raw.clone()])
                        .collect::<Vec<_>>(),
                    &text[record.terminator.clone()],
                )
            })
            .collect::<Vec<_>>();
        let wanted = expected
            .iter()
            .map(|(cells, terminator)| (cells.to_vec(), *terminator))
            .collect::<Vec<_>>();
        if read != wanted {
            return Err(format!("read {read:#?}"));
        }
        for record in &table.records {
            let mut at = record.raw.start;
            for (index, cell) in record.cells.iter().enumerate() {
                if cell.raw.start != at {
                    return Err(format!("cell starts at {} not {at}", cell.raw.start));
                }
                at = cell
                    .raw
                    .end
                    .saturating_add(usize::from(index + 1 < record.cells.len()));
            }
            if record.cells.last().map(|cell| cell.raw.end) != Some(record.raw.end) {
                return Err("the last cell does not end its record".to_string());
            }
        }
        Ok(())
    }

    const MAIN_EXPECTED: Expected<'static> = &[
        (
            &[
                "computer",
                "AI login email",
                "services",
                "codes arrive in (my guess)",
                "why I guessed",
                "your correction (leave blank if my guess is right)",
            ],
            "\n",
        ),
        (
            &[
                "m1",
                "alice@example.test",
                "Claude",
                "email",
                "\"The sign-in page said \"\"check your inbox\"\", so email.\"",
                "",
            ],
            "\n",
        ),
        (
            &[
                "m2",
                "bob@example.test",
                "\"Claude, GitHub\"",
                "SMS",
                "\"Two reasons: first\nthe phone prompt, then the code\"",
                "",
            ],
            "\n",
        ),
        (
            &["m3", "carol@example.test", "Claude", "authenticator"],
            "\n",
        ),
        (&["m4", "dave@example.test", "", "", "", "", "extra"], "\n"),
    ];

    const MIXED_EXPECTED: Expected<'static> = &[
        (&["computer", "email", "why", "fix"], "\r\n"),
        (&["m1", "erin@example.test", "\"one\nand two\"", ""], "\r\n"),
        (
            &["m2", "frank@example.test", "\"Said \"\"no\"\"\"", "later"],
            "",
        ),
    ];

    const ALL_CRLF_EXPECTED: Expected<'static> = &[
        (&["computer", "email", "why", "fix"], "\r\n"),
        (
            &["m1", "gina@example.test", "\"line one\r\nline two\"", ""],
            "\r\n",
        ),
        (&["m2", "hal@example.test", "plain", ""], "\r\n"),
    ];

    const STRAY_EXPECTED: Expected<'static> = &[
        (&["a", "b\"c", "d"], "\n"),
        (&["\"x\"y", "z"], "\n"),
        (&["\"never closes,\nstill open"], ""),
    ];

    const TSV_EXPECTED: Expected<'static> = &[
        (
            &[
                "computer",
                "AI login email",
                "services",
                "why I guessed",
                "your correction",
            ],
            "\n",
        ),
        (
            &[
                "m1",
                "alice@example.test",
                "Claude, GitHub",
                "said \"inbox\", so email",
                "",
            ],
            "\n",
        ),
        (
            &["m2", "bob@example.test", "Claude", "\"Two\tparts\"", ""],
            "\n",
        ),
    ];

    fn fixtures() -> [(&'static str, Delimiter, Expected<'static>); 5] {
        [
            (MAIN, Delimiter::Comma, MAIN_EXPECTED),
            (MIXED, Delimiter::Comma, MIXED_EXPECTED),
            (ALL_CRLF, Delimiter::Comma, ALL_CRLF_EXPECTED),
            (STRAY, Delimiter::Comma, STRAY_EXPECTED),
            (TSV, Delimiter::Tab, TSV_EXPECTED),
        ]
    }

    /// A scanner that ends a record at a line break inside quotes: the NEGATIVE CONTROL of T1.
    fn scan_breaking_inside_quotes(text: &str, delimiter: Delimiter) -> Result<Table, TooLarge> {
        let mut records = Vec::new();
        let mut start = 0;
        for line in text.split_inclusive('\n') {
            let body = line.trim_end_matches(['\n', '\r']);
            let mut cells = Vec::new();
            let mut at = start;
            for part in body.split(delimiter.char()) {
                cells.push(Cell {
                    raw: at..at + part.len(),
                    content: at..at + part.len(),
                    after: at + part.len()..at + part.len(),
                    quoted: part.starts_with('"'),
                });
                at += part.len() + 1;
            }
            records.push(Record {
                raw: start..start + body.len(),
                cells,
                terminator: start + body.len()..start + line.len(),
            });
            start += line.len();
        }
        Ok(Table {
            delimiter,
            records,
            problems: Vec::new(),
            columns: 0,
            widths: OnceLock::new(),
        })
    }

    /// T1: every fixture cell's raw bytes, every record's terminator, and every value.
    #[test]
    fn t1_scan_reads_every_fixture_cell() {
        for (text, delimiter, expected) in fixtures() {
            check_scan(Table::scan, text, delimiter, expected)
                .unwrap_or_else(|error| panic!("{text:?}: {error}"));
        }
        let table = Table::scan(MAIN, Delimiter::Comma).unwrap();
        assert_eq!(table.columns(), 7);
        let value = |record: usize, column: usize| {
            table.records[record].cells[column].value(MAIN).into_owned()
        };
        assert_eq!(
            value(1, 4),
            "The sign-in page said \"check your inbox\", so email."
        );
        assert_eq!(value(2, 2), "Claude, GitHub");
        assert_eq!(
            value(2, 4),
            "Two reasons: first\nthe phone prompt, then the code"
        );
        assert_eq!(value(1, 5), "");
        let stray = Table::scan(STRAY, Delimiter::Comma).unwrap();
        assert_eq!(stray.records[0].cells[1].value(STRAY), "b\"c");
        assert_eq!(stray.records[1].cells[0].value(STRAY), "xy");
        assert_eq!(
            stray
                .problems
                .iter()
                .map(|problem| problem.kind)
                .collect::<Vec<_>>(),
            [ProblemKind::TextAfterQuote, ProblemKind::UnclosedQuote]
        );
        assert_eq!(stray.first_problem_line(STRAY), Some(2));
        let tsv = Table::scan(TSV, Delimiter::Tab).unwrap();
        assert_eq!(tsv.records[2].cells[3].value(TSV), "Two\tparts");
        let empty = Table::scan("", Delimiter::Comma).unwrap();
        assert_eq!(empty.records.len(), 1);
        assert_eq!(empty.records[0].cells.len(), 1);
        let blank_line = Table::scan("a\n\nb\n", Delimiter::Comma).unwrap();
        assert_eq!(
            blank_line.records.len(),
            3,
            "a blank line is a record of one empty cell"
        );

        // NEGATIVE CONTROL: a scanner that ends a record at a newline inside quotes fails.
        assert!(
            check_scan(
                scan_breaking_inside_quotes,
                MAIN,
                Delimiter::Comma,
                MAIN_EXPECTED
            )
            .is_err()
        );
        assert!(
            check_scan(
                scan_breaking_inside_quotes,
                MIXED,
                Delimiter::Comma,
                MIXED_EXPECTED
            )
            .is_err()
        );
    }

    #[test]
    fn the_caps_open_a_table_as_text() {
        for delimiter in [Delimiter::Comma, Delimiter::Tab] {
            let separator = delimiter.char().to_string();
            for final_cell in ["x", "\"x\"", "\""] {
                let at_limit = format!("{}{final_cell}", separator.repeat(MAX_TABLE_COLUMNS - 1));
                let table = Table::scan(&at_limit, delimiter).unwrap();
                assert_eq!(table.columns(), MAX_TABLE_COLUMNS);
                let last = table.records[0].cells.last().unwrap();
                assert_eq!(&at_limit[last.raw.clone()], final_cell);
                assert_eq!(
                    last.value(&at_limit),
                    if final_cell == "\"" { "" } else { "x" }
                );
                assert_eq!(
                    table.problems.len(),
                    usize::from(final_cell == "\""),
                    "an unfinished quote stays tolerated at the column limit"
                );

                let wide = format!("{}{final_cell}", separator.repeat(MAX_TABLE_COLUMNS));
                assert_eq!(
                    Table::scan(&wide, delimiter).map(|table| table.columns()),
                    Err(TooLarge::Columns),
                    "{delimiter:?}, final cell {final_cell:?}"
                );
                if final_cell == "\"" {
                    let mut store = crate::store::DocumentStore::for_test();
                    let document = store.open("file:///synthetic/columns.csv".into(), wide.clone());
                    let before = store.snapshot(document).unwrap();
                    let mut workspace = crate::editor::EditorWorkspace::new();
                    let mut view = workspace
                        .attach(&mut store, document, crate::store::DocumentViewId(1))
                        .unwrap();
                    assert_eq!(
                        workspace.set_face(&store, &mut view, Some(delimiter)),
                        Err(TooLarge::Columns.reason())
                    );
                    assert!(view.grid().is_none());
                    let after = store.snapshot(document).unwrap();
                    assert_eq!(after.seq, before.seq);
                    assert_eq!(after.text.as_ref(), wide.as_str());
                }
            }
        }
        let tall = "x\n".repeat(MAX_TABLE_RECORDS + 1);
        assert_eq!(
            Table::scan(&tall, Delimiter::Comma).unwrap_err(),
            TooLarge::Records
        );
        let long = "x".repeat(MAX_TABLE_BYTES + 1);
        assert!(matches!(
            Table::scan(&long, Delimiter::Comma),
            Err(TooLarge::Bytes(_))
        ));
        assert!(TooLarge::Bytes(5 << 20).reason().contains("5.0 MiB"));
        assert!(Table::scan(&"x\n".repeat(MAX_TABLE_RECORDS), Delimiter::Comma).is_ok());
    }

    #[test]
    fn record_and_cell_at_find_the_caret() {
        let table = Table::scan(MAIN, Delimiter::Comma).unwrap();
        for (r, record) in table.records.iter().enumerate() {
            for (c, cell) in record.cells.iter().enumerate() {
                for at in [cell.raw.start, cell.content.start, cell.raw.end] {
                    assert_eq!(table.record_at(at), r, "{at}");
                    if at != cell.raw.end || c + 1 == record.cells.len() || cell.raw.is_empty() {
                        assert_eq!(table.cell_at(r, at), c, "{r}:{c} at {at}");
                    }
                }
                assert_eq!(table.record_at(table.caret_for(r, c)), r);
                assert_eq!(table.cell_at(r, table.caret_for(r, c)), c);
            }
            assert_eq!(table.record_at(record.terminator.start), r);
        }
        assert_eq!(table.record_at(MAIN.len()), table.records.len() - 1);
        assert_eq!(table.caret_for(3, 5), table.records[3].raw.end);
    }

    /// The encoder that always quotes, as jq's `@csv` does: the NEGATIVE CONTROL of T2.
    fn encode_always_quoting(value: &str, _: Delimiter, _: bool) -> Cow<'_, str> {
        Cow::Owned(format!("\"{}\"", value.replace('"', "\"\"")))
    }

    /// Whether re-encoding every regular cell's own value with `encoder` writes its bytes back.
    fn check_requote(
        encoder: impl Fn(&str, Delimiter, bool) -> Cow<'_, str>,
    ) -> Result<(), String> {
        for (text, delimiter, _) in fixtures() {
            let table = Table::scan(text, delimiter).unwrap();
            for record in &table.records {
                for cell in &record.cells {
                    let raw = &text[cell.raw.clone()];
                    let irregular = !cell.after.is_empty()
                        || (!cell.quoted && raw.contains('"'))
                        || table.problems.iter().any(|problem| cell.holds(problem.at));
                    if irregular {
                        continue;
                    }
                    let value = cell.value(text);
                    let written = encoder(&value, delimiter, cell.quoted);
                    if written != raw {
                        return Err(format!("{raw:?} re-encodes as {written:?}"));
                    }
                }
            }
        }
        Ok(())
    }

    /// T2: committing a cell's own value is no edit, and a regular cell's value re-encodes to
    /// its own bytes.
    #[test]
    fn t2_a_cells_own_value_is_no_edit() {
        for (text, delimiter, _) in fixtures() {
            let table = Table::scan(text, delimiter).unwrap();
            for (r, record) in table.records.iter().enumerate() {
                for (c, cell) in record.cells.iter().enumerate() {
                    assert_eq!(
                        edit_cell(&table, text, r, c, &cell.value(text)),
                        Ok(None),
                        "{text:?} {r}:{c}"
                    );
                }
                for c in record.cells.len()..table.columns() {
                    assert_eq!(
                        edit_cell(&table, text, r, c, ""),
                        Ok(None),
                        "{r}:{c} missing"
                    );
                }
            }
        }
        check_requote(encode).unwrap();
        // NEGATIVE CONTROL: an always-quote encoder rewrites bare cells.
        assert!(check_requote(encode_always_quoting).is_err());
    }

    /// The values T3 puts in every cell.
    const TRICKY: &[&str] = &[
        "a,b",
        "say \"hi\"",
        "two\nlines",
        "",
        " leading space",
        "中文字",
        "👩\u{200d}💻 ok",
        "tab\there",
        "plain",
    ];

    /// A commit that rewrites the whole record in canonical quoting: the NEGATIVE CONTROL of T3.
    fn edit_rewriting_the_record(
        table: &Table,
        text: &str,
        record: usize,
        column: usize,
        value: &str,
    ) -> Result<Option<TextEdit>, Unwritable> {
        let Some(row) = table.records.get(record) else {
            return Ok(None);
        };
        let mut values = row
            .cells
            .iter()
            .map(|cell| cell.value(text).into_owned())
            .collect::<Vec<_>>();
        if values.len() <= column {
            values.resize(column + 1, String::new());
        }
        if values[column] == value {
            return Ok(None);
        }
        values[column] = value.to_string();
        let insert = values
            .iter()
            .map(|value| encode(value, table.delimiter, false).into_owned())
            .collect::<Vec<_>>()
            .join(&table.delimiter.char().to_string());
        Ok(Some(TextEdit {
            range: row.raw.clone(),
            insert,
        }))
    }

    type CellEditor = fn(&Table, &str, usize, usize, &str) -> Result<Option<TextEdit>, Unwritable>;

    /// Whether `editor` puts every tricky value in every cell (and one past a short row's end)
    /// so a rescan reads it back, every other cell keeps its value, and every byte outside the
    /// cell is unchanged.
    fn check_locality(editor: CellEditor) -> Result<(), String> {
        for (text, delimiter, _) in fixtures() {
            let table = Table::scan(text, delimiter).unwrap();
            for (r, record) in table.records.iter().enumerate() {
                for c in 0..table.columns().max(record.cells.len()) {
                    if table.unclosed(r) && c >= record.cells.len() {
                        // The open quote would swallow a cell added after it.
                        if editor(&table, text, r, c, "x") != Err(Unwritable::UnclosedRow) {
                            return Err(format!("{r}:{c} was added inside an open quote"));
                        }
                        continue;
                    }
                    if table.cell_unclosed(r, c) {
                        // The cell runs to the end of the text: writing it would drop the rest.
                        if editor(&table, text, r, c, "x") != Err(Unwritable::UnclosedCell) {
                            return Err(format!("{r}:{c} rewrote a cell that never closes"));
                        }
                        continue;
                    }
                    for value in TRICKY {
                        let Ok(edit) = editor(&table, text, r, c, value) else {
                            return Err(format!("{r}:{c} {value:?} was refused"));
                        };
                        let Some(edit) = edit else {
                            let current = table
                                .cell(r, c)
                                .map_or(String::new(), |cell| cell.value(text).into_owned());
                            if current != *value {
                                return Err(format!("{r}:{c} {value:?} made no edit"));
                            }
                            continue;
                        };
                        let own = table
                            .cell(r, c)
                            .map_or(record.raw.end..record.raw.end, |cell| cell.raw.clone());
                        if edit.range != own {
                            return Err(format!(
                                "{text:?} {r}:{c} {value:?}: edit covers {:?}, the cell is {own:?}",
                                edit.range
                            ));
                        }
                        let mut after = text.to_string();
                        after.replace_range(edit.range.clone(), &edit.insert);
                        if after[..edit.range.start] != text[..edit.range.start]
                            || after[edit.range.start + edit.insert.len()..]
                                != text[edit.range.end..]
                        {
                            return Err(format!("{r}:{c} {value:?} moved bytes outside the cell"));
                        }
                        let rescan = Table::scan(&after, delimiter).unwrap();
                        let read = rescan
                            .cell(r, c)
                            .map(|cell| cell.value(&after).into_owned());
                        if read.as_deref() != Some(*value) {
                            return Err(format!("{r}:{c} {value:?} reads back as {read:?}"));
                        }
                        for (rr, other) in table.records.iter().enumerate() {
                            for (cc, cell) in other.cells.iter().enumerate() {
                                if (rr, cc) == (r, c) {
                                    continue;
                                }
                                let kept = rescan.cell(rr, cc).map(|cell| cell.value(&after));
                                if kept.as_deref() != Some(&*cell.value(text)) {
                                    return Err(format!(
                                        "{r}:{c} {value:?} changed {rr}:{cc} to {kept:?}"
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// T3: a cell edit changes that cell's bytes and nothing else.
    #[test]
    fn t3_a_commit_touches_only_its_cell() {
        check_locality(edit_cell).unwrap();
        // NEGATIVE CONTROL: rewriting the whole record in canonical quoting moves bytes.
        assert!(check_locality(edit_rewriting_the_record).is_err());
    }

    /// A commit to a missing cell that pads every row out to the header's width: the NEGATIVE
    /// CONTROL of T4.
    fn edit_padding_every_row(
        table: &Table,
        text: &str,
        record: usize,
        column: usize,
        value: &str,
    ) -> Result<Option<TextEdit>, Unwritable> {
        let Some(mut edit) = edit_cell(table, text, record, column, value)? else {
            return Ok(None);
        };
        if table.cell(record, column).is_some() {
            return Ok(Some(edit));
        }
        let header = table.records[0].cells.len();
        let mut padded = String::new();
        for (r, row) in table.records.iter().enumerate() {
            padded.push_str(&text[row.raw.clone()]);
            if r == record {
                padded.push_str(&edit.insert);
            } else {
                for _ in row.cells.len()..header {
                    padded.push(table.delimiter.char());
                }
            }
            padded.push_str(&text[row.terminator.clone()]);
        }
        edit.range = 0..text.len();
        edit.insert = padded;
        Ok(Some(edit))
    }

    /// Whether `editor` fills m3's missing last cell, and a short row's missing cell, by padding
    /// only that row.
    fn check_ragged(editor: CellEditor) -> Result<(), String> {
        const RAGGED: &str = "a,b,c\n1\n2,3\n4,5,6\n";
        let table = Table::scan(RAGGED, Delimiter::Comma).unwrap();
        let edit = editor(&table, RAGGED, 1, 2, "x")
            .ok()
            .flatten()
            .ok_or("no edit")?;
        let mut after = RAGGED.to_string();
        after.replace_range(edit.range.clone(), &edit.insert);
        if after != "a,b,c\n1,,x\n2,3\n4,5,6\n" {
            return Err(format!("wrote {after:?}"));
        }
        let table = Table::scan(MAIN, Delimiter::Comma).unwrap();
        let edit = editor(&table, MAIN, 3, 5, "fix, \"quoted\"")
            .ok()
            .flatten()
            .ok_or("no edit")?;
        let mut after = MAIN.to_string();
        after.replace_range(edit.range.clone(), &edit.insert);
        let expected = MAIN.replace(
            "m3,carol@example.test,Claude,authenticator\n",
            "m3,carol@example.test,Claude,authenticator,,\"fix, \"\"quoted\"\"\"\n",
        );
        if after != expected {
            return Err(format!("wrote {after:?}"));
        }
        if editor(&table, MAIN, 3, 4, "") != Ok(None) {
            return Err("an empty value in a missing cell made an edit".to_string());
        }
        Ok(())
    }

    /// T4: a missing cell pads only its own row, and an empty value there is no edit.
    #[test]
    fn t4_a_missing_cell_pads_only_its_row() {
        check_ragged(edit_cell).unwrap();
        // NEGATIVE CONTROL: padding every row to the header's width fails.
        assert!(check_ragged(edit_padding_every_row).is_err());
    }

    /// The edit `edit_cell` makes, applied to `text`.
    fn applied(text: &str, record: usize, column: usize, value: &str) -> String {
        let table = Table::scan(text, Delimiter::Comma).unwrap();
        let mut after = text.to_string();
        if let Some(edit) = edit_cell(&table, text, record, column, value).unwrap() {
            after.replace_range(edit.range, &edit.insert);
        }
        after
    }

    /// A cell whose quote never closes holds every later row: it is not written, so no clear,
    /// paste or commit on it can drop those rows (the review saw one Backspace empty the file).
    #[test]
    fn a_cell_that_never_closes_is_not_written() {
        const UNCLOSED: &str = concat!(
            "computer,email,why,fix\n",
            "m1,alice@example.test,\"5 inch screen, so SMS,\n",
            "m2,bob@example.test,plain,\n",
            "m3,carol@example.test,plain,\n",
        );
        let table = Table::scan(UNCLOSED, Delimiter::Comma).unwrap();
        assert_eq!(table.records.len(), 2, "the open quote swallows m2 and m3");
        assert!(table.cell_unclosed(1, 2));
        assert!(!table.cell_unclosed(1, 1));
        for value in ["", "x", "5 inch screen, so SMS"] {
            assert_eq!(
                edit_cell(&table, UNCLOSED, 1, 2, value),
                Err(Unwritable::UnclosedCell),
                "{value:?}"
            );
        }
        let own = table.cell(1, 2).unwrap().value(UNCLOSED).into_owned();
        assert_eq!(edit_cell(&table, UNCLOSED, 1, 2, &own), Ok(None));
        // The cells before it are still written in place.
        assert_eq!(
            applied(UNCLOSED, 1, 1, "erin@example.test"),
            UNCLOSED.replace("alice@", "erin@")
        );
    }

    /// Clearing a record's only cell writes `""`: a bare empty value would turn the row into a
    /// blank line that CSV readers skip, or, as the last record with no final newline, into
    /// the file's final line break (the row gone on the rescan).
    #[test]
    fn clearing_a_records_only_cell_keeps_the_row() {
        for (text, cleared) in [
            ("name\nalice\nbob", "name\nalice\n\"\""),
            ("name\nalice\nbob\n", "name\nalice\n\"\"\n"),
            ("name\nalice\nbob\n", "name\n\"\"\nbob\n"),
        ] {
            let record = if cleared.starts_with("name\n\"") {
                1
            } else {
                2
            };
            let after = applied(text, record, 0, "");
            assert_eq!(after, cleared, "{text:?}");
            let rescan = Table::scan(&after, Delimiter::Comma).unwrap();
            assert_eq!(rescan.records.len(), 3, "{after:?}");
            assert_eq!(rescan.cell(record, 0).unwrap().value(&after), "");
        }
        // A record of more cells keeps an empty value bare.
        assert_eq!(applied("a,b\n1,2\n", 1, 1, ""), "a,b\n1,\n");
        let tsv = "name\nbob";
        let table = Table::scan(tsv, Delimiter::Tab).unwrap();
        let edit = edit_cell(&table, tsv, 1, 0, "").unwrap().unwrap();
        assert_eq!(edit.insert, "\"\"");
    }

    #[test]
    fn encode_quotes_only_what_needs_it() {
        let comma = Delimiter::Comma;
        assert_eq!(encode("plain", comma, false), "plain");
        assert_eq!(encode("", comma, false), "");
        assert_eq!(encode("", comma, true), "\"\"");
        assert_eq!(encode("plain", comma, true), "\"plain\"");
        assert_eq!(encode("a,b", comma, false), "\"a,b\"");
        assert_eq!(encode("say \"hi\"", comma, false), "\"say \"\"hi\"\"\"");
        assert_eq!(encode("a\nb", comma, false), "\"a\nb\"");
        assert_eq!(encode("a\rb", comma, false), "\"a\rb\"");
        assert_eq!(encode("a,b", Delimiter::Tab, false), "a,b");
        assert_eq!(encode("a\tb", Delimiter::Tab, false), "\"a\tb\"");
        assert_eq!(encode(" lead", comma, false), " lead");
    }

    #[test]
    fn widths_clip_and_wrap_lay_out_cells() {
        let table = Table::scan(MAIN, Delimiter::Comma).unwrap();
        let widths = table.column_widths(MAIN);
        assert_eq!(widths.len(), 7);
        assert_eq!(widths[0], "computer".len());
        assert_eq!(widths[5], MAX_COLUMN_WIDTH, "the long header caps");
        assert_eq!(widths[6], "extra".len());
        assert!(
            widths
                .iter()
                .all(|w| (MIN_COLUMN_WIDTH..=MAX_COLUMN_WIDTH).contains(w))
        );

        assert_eq!(clip("Claude", 8), "Claude");
        assert_eq!(clip("Claude, GitHub", 8), "Claude,…");
        assert_eq!(clip("first\nsecond", 8), "first⏎");
        assert_eq!(clip("first line\nsecond", 6), "first…");
        assert_eq!(clip("中文字中文字", 5), "中文…");
        assert_eq!(clip("👩\u{200d}💻👩\u{200d}💻", 3), "👩\u{200d}💻…");
        assert_eq!(clip("a\tb", 5), "a b");
        assert_eq!(clip("anything", 0), "");
        assert_eq!(clip("ab", 1), "…");

        let value = "Two reasons: first\nthe phone prompt, then the code";
        let lines = wrap(value, 12);
        let shown = lines
            .iter()
            .map(|line| &value[line.clone()])
            .collect::<Vec<_>>();
        assert_eq!(
            shown,
            [
                "Two reasons: ",
                "first",
                "the phone ",
                "prompt, then ",
                "the code"
            ]
        );
        assert!(shown.iter().all(|line| text_width(line.trim_end()) <= 12));
        assert_eq!(wrap("", 10), vec![0..0]);
        assert_eq!(wrap("a\n", 10), [0..1, 2..2]);
        assert_eq!(wrap("a\r\nb", 10), [0..1, 3..4]);
        assert_eq!(wrap("abcdef", 2), [0..2, 2..4, 4..6]);
        assert_eq!(wrapped_caret(value, &lines, 0), (0, 0));
        assert_eq!(
            wrapped_caret(value, &lines, 13),
            (1, 0),
            "a soft break's end is the next line"
        );
        assert_eq!(
            wrapped_caret(value, &lines, 18),
            (1, 5),
            "the newline ends its line"
        );
        assert_eq!(wrapped_caret(value, &lines, value.len()), (4, 8));
    }

    /// A row inserter under test: the shipped [`insert_row`] or a rig.
    type RowInserter = fn(&Table, &str, usize) -> Result<Option<TextEdit>, Unwritable>;

    /// A row deleter under test: the shipped [`delete_row`] or a rig.
    type RowDeleter = fn(&Table, &str, usize) -> Result<Option<TextEdit>, Unwritable>;

    /// The inserter that ends every new row with `\n`, whatever the file's own breaks: the
    /// NEGATIVE CONTROL of the row tests, which the mixed fixture's CRLF records catch.
    fn insert_row_always_lf(
        table: &Table,
        _: &str,
        record: usize,
    ) -> Result<Option<TextEdit>, Unwritable> {
        let row = &table.records[record];
        let empty = table
            .delimiter
            .char()
            .to_string()
            .repeat(table.records[0].cells.len() - 1);
        Ok(Some(if row.terminator.is_empty() {
            TextEdit {
                range: row.raw.end..row.raw.end,
                insert: format!("\n{empty}"),
            }
        } else {
            TextEdit {
                range: row.terminator.end..row.terminator.end,
                insert: format!("{empty}\n"),
            }
        }))
    }

    /// The deleter that takes a row and its own break, even a last row with none: the NEGATIVE
    /// CONTROL of the delete test, which leaves a file that had no final newline with one.
    fn delete_row_and_its_break(
        table: &Table,
        _: &str,
        record: usize,
    ) -> Result<Option<TextEdit>, Unwritable> {
        let row = &table.records[record];
        Ok(Some(TextEdit {
            range: row.raw.start..row.terminator.end,
            insert: String::new(),
        }))
    }

    /// Each fixture's cells as values, and each record's line break, for comparing a rescan.
    fn read_back(text: &str, table: &Table) -> Vec<(Vec<String>, String)> {
        table
            .records
            .iter()
            .map(|record| {
                (
                    record
                        .cells
                        .iter()
                        .map(|cell| cell.value(text).into_owned())
                        .collect(),
                    text[record.terminator.clone()].to_string(),
                )
            })
            .collect()
    }

    /// The fixtures a row operation runs on: every one the scan reads without a problem (the
    /// CRLF, mixed and no-final-newline ones among them).
    fn row_fixtures() -> Vec<(&'static str, Delimiter)> {
        fixtures()
            .into_iter()
            .filter(|(text, delimiter, _)| {
                Table::scan(text, *delimiter).unwrap().problems.is_empty()
            })
            .map(|(text, delimiter, _)| (text, delimiter))
            .collect()
    }

    /// Whether `insert` adds, after every record of every fixture, one row of the header's number
    /// of empty cells ending with the line break of the row above, as one insertion at the
    /// boundary that moves no other byte; after a last row with no break, the break before it is
    /// the file's own and the text still ends without one.
    fn check_insert_rows(insert: RowInserter) -> Result<(), String> {
        for (text, delimiter) in row_fixtures() {
            let table = Table::scan(text, delimiter).unwrap();
            let before = read_back(text, &table);
            let header = table.records[0].cells.len();
            for record in 0..table.records.len() {
                let edit = insert(&table, text, record)
                    .map_err(|why| format!("{why:?}"))?
                    .ok_or_else(|| format!("no row after {record}"))?;
                let row = &table.records[record];
                let boundary = if row.terminator.is_empty() {
                    row.raw.end
                } else {
                    row.terminator.end
                };
                if edit.range != (boundary..boundary) {
                    return Err(format!("{text:?} row {record}: edit {:?}", edit.range));
                }
                let mut after = text.to_string();
                after.replace_range(edit.range.clone(), &edit.insert);
                let rescan = Table::scan(&after, delimiter).unwrap();
                let mut now = read_back(&after, &rescan);
                if now.len() != before.len() + 1 {
                    return Err(format!("{after:?}: {} records", now.len()));
                }
                let (cells, terminator) = now.remove(record + 1);
                if cells.len() != header || cells.iter().any(|cell| !cell.is_empty()) {
                    return Err(format!("{after:?}: the new row reads {cells:?}"));
                }
                let above = before[record].1.as_str();
                if terminator != above {
                    return Err(format!(
                        "{text:?} row {record}: the new row ends {terminator:?}, the row above {above:?}"
                    ));
                }
                let mut expected = before.clone();
                if above.is_empty() {
                    let earlier = before[..record]
                        .iter()
                        .rev()
                        .map(|(_, terminator)| terminator.as_str())
                        .find(|terminator| !terminator.is_empty())
                        .unwrap_or("\n");
                    expected[record].1 = earlier.to_string();
                }
                if now != expected {
                    return Err(format!("{after:?}: the other rows read {now:?}"));
                }
            }
        }
        Ok(())
    }

    /// Tables with a blank line before a last row that has no line break, LF and CRLF: deleting
    /// that row keeps the blank line, which needs its break.
    const BLANK_BEFORE_LAST: [&str; 2] = ["a,b\n1,2\n\n3,4", "a,b\r\n1,2\r\n\r\n3,4"];

    /// Whether `delete` takes each record of every fixture and nothing else: every other row
    /// reads as before, with its own line break, the bytes it removes are the row's own (and, for
    /// a last row with no line break, the break before it), and the text ends with a line break
    /// exactly when it did, unless the row before a last row with none is a blank line (which
    /// ends the text only with its break).
    fn check_delete_rows(delete: RowDeleter) -> Result<(), String> {
        let blank_before_last = BLANK_BEFORE_LAST.map(|text| (text, Delimiter::Comma));
        for (text, delimiter) in row_fixtures().into_iter().chain(blank_before_last) {
            let table = Table::scan(text, delimiter).unwrap();
            let before = read_back(text, &table);
            for record in 0..table.records.len() {
                let edit = delete(&table, text, record)
                    .map_err(|why| format!("{why:?}"))?
                    .ok_or_else(|| format!("nothing deleted at {record}"))?;
                let row = &table.records[record];
                let floor = record
                    .checked_sub(1)
                    .map_or(row.raw.start, |at| table.records[at].terminator.start);
                if !edit.insert.is_empty()
                    || edit.range.start < floor
                    || edit.range.end > row.terminator.end
                    || edit.range.start > row.raw.start
                    || edit.range.end < row.raw.end
                {
                    return Err(format!("{text:?} row {record}: edit {edit:?}"));
                }
                let mut after = text.to_string();
                after.replace_range(edit.range.clone(), "");
                let blank_above = row.terminator.is_empty()
                    && record
                        .checked_sub(1)
                        .is_some_and(|at| table.records[at].raw.is_empty());
                if !blank_above && after.ends_with(['\r', '\n']) != text.ends_with(['\r', '\n']) {
                    return Err(format!(
                        "{text:?} row {record}: the final break changed: {after:?}"
                    ));
                }
                let rescan = Table::scan(&after, delimiter).unwrap();
                let now = read_back(&after, &rescan);
                let mut expected = before.clone();
                expected.remove(record);
                // A last row with no break takes the one above it, unless that row is blank.
                if let Some(last) = expected.last_mut()
                    && row.terminator.is_empty()
                    && !blank_above
                {
                    last.1.clear();
                }
                if now != expected {
                    return Err(format!("{after:?}: the rows read {now:?}"));
                }
            }
        }
        Ok(())
    }

    /// G2 rows: an added row has the header's number of empty cells and the line break of the
    /// row above (a break before it when the file had no final newline), and a deleted row takes
    /// only its own bytes, on the CRLF, mixed and no-final-newline fixtures. *Controls:* a rig
    /// that always appends `\n` fails the mixed fixture, and one that leaves a final newline
    /// fails the no-final-newline fixture.
    #[test]
    fn rows_are_added_and_deleted_in_their_own_bytes() {
        check_insert_rows(insert_row).unwrap();
        check_delete_rows(delete_row).unwrap();
        let control = check_insert_rows(insert_row_always_lf).unwrap_err();
        assert!(
            control.contains("\\r\\n"),
            "the mixed fixture's CRLF catches it: {control}"
        );
        let control = check_delete_rows(delete_row_and_its_break).unwrap_err();
        assert!(control.contains("final break"), "{control}");

        // The mixed file's last row has no line break: a row added after it brings the file's
        // CRLF before it, and the file still ends without one.
        let table = Table::scan(MIXED, Delimiter::Comma).unwrap();
        let edit = insert_row(&table, MIXED, 2).unwrap().unwrap();
        assert_eq!(edit.insert, "\r\n,,,");
        let edit = insert_row(&table, MIXED, 1).unwrap().unwrap();
        assert_eq!(edit.insert, ",,,\r\n");
        // A one-column table's empty row is `""`, not a blank line readers skip.
        let one = "name\nm1\n";
        let table = Table::scan(one, Delimiter::Comma).unwrap();
        assert_eq!(
            insert_row(&table, one, 0).unwrap().unwrap().insert,
            "\"\"\n"
        );
        // A row whose quote never closes holds every row after it: no row is added after it or
        // deleted with it, and the rows before it still are.
        let table = Table::scan(STRAY, Delimiter::Comma).unwrap();
        let last = table.records.len() - 1;
        assert_eq!(
            insert_row(&table, STRAY, last),
            Err(Unwritable::UnclosedRow)
        );
        assert_eq!(
            delete_row(&table, STRAY, last),
            Err(Unwritable::UnclosedRow)
        );
        assert!(insert_row(&table, STRAY, 0).unwrap().is_some());
        assert!(delete_row(&table, STRAY, 0).unwrap().is_some());
        assert_eq!(insert_row(&table, STRAY, last + 1), Ok(None));
        assert_eq!(delete_row(&table, STRAY, last + 1), Ok(None));
    }

    /// G2 review: a last row with no line break after a blank line goes alone, the blank line
    /// and its break staying (LF and CRLF). *Control:* the deleter before this fix took the blank
    /// line's break, so one delete took two rows; `check_delete_rows` with it fails on
    /// [`BLANK_BEFORE_LAST`].
    #[test]
    fn deleting_a_last_row_keeps_the_blank_line_above_it() {
        check_delete_rows(delete_row).unwrap();
        for text in BLANK_BEFORE_LAST {
            let table = Table::scan(text, Delimiter::Comma).unwrap();
            let edit = delete_row(&table, text, 3).unwrap().unwrap();
            let mut after = text.to_string();
            after.replace_range(edit.range, "");
            let rescan = Table::scan(&after, Delimiter::Comma).unwrap();
            assert_eq!(rescan.records.len(), 3, "{after:?}");
            assert!(rescan.records[2].raw.is_empty(), "{after:?}");
        }
        let control = check_delete_rows(delete_row_taking_the_break_above).unwrap_err();
        assert!(control.contains("the rows read"), "{control}");
    }

    /// The deleter before the blank-line fix: a last row with no break always takes the break
    /// above it. The control of [`deleting_a_last_row_keeps_the_blank_line_above_it`].
    fn delete_row_taking_the_break_above(
        table: &Table,
        _: &str,
        record: usize,
    ) -> Result<Option<TextEdit>, Unwritable> {
        let row = &table.records[record];
        let range = match record.checked_sub(1).map(|at| &table.records[at]) {
            Some(previous) if row.terminator.is_empty() => previous.terminator.start..row.raw.end,
            _ => row.raw.start..row.terminator.end,
        };
        Ok(Some(TextEdit {
            range,
            insert: String::new(),
        }))
    }

    /// G2 review: deleting a row between a lone `\r` and a blank `\n` line is refused, as the two
    /// breaks would read as one `\r\n` and the blank line would go too; the file's other rows
    /// still delete. *Control:* deleting the row's own bytes there leaves two rows fewer.
    #[test]
    fn a_delete_that_would_join_a_cr_and_a_blank_lf_line_is_refused() {
        let text = "a,b\r1,2\n\n3,4\n";
        let table = Table::scan(text, Delimiter::Comma).unwrap();
        assert_eq!(table.records.len(), 4);
        assert_eq!(
            delete_row(&table, text, 1),
            Err(Unwritable::JoinsLineBreaks)
        );
        for record in [0, 2, 3] {
            let edit = delete_row(&table, text, record).unwrap().unwrap();
            let mut after = text.to_string();
            after.replace_range(edit.range, "");
            assert_eq!(
                Table::scan(&after, Delimiter::Comma).unwrap().records.len(),
                3,
                "row {record}: {after:?}"
            );
        }
        let own = table.records[1].raw.start..table.records[1].terminator.end;
        let mut joined = text.to_string();
        joined.replace_range(own, "");
        assert_eq!(joined, "a,b\r\n3,4\n");
        assert_eq!(
            Table::scan(&joined, Delimiter::Comma)
                .unwrap()
                .records
                .len(),
            2,
            "the control: the plain delete takes the blank line too"
        );
    }

    #[test]
    fn the_memo_scans_once_per_revision() {
        let mut store = crate::store::DocumentStore::for_test();
        let document = store.open("mem://t.csv".into(), MAIN.into());
        let snapshot = store.snapshot(document).unwrap();
        let mut memo = TableMemo::default();
        let first = memo
            .table(document, snapshot.seq, Delimiter::Comma, &snapshot.text)
            .unwrap();
        let again = memo
            .table(document, snapshot.seq, Delimiter::Comma, &snapshot.text)
            .unwrap();
        assert!(Arc::ptr_eq(&first, &again));
        let tab = memo
            .table(document, snapshot.seq, Delimiter::Tab, &snapshot.text)
            .unwrap();
        assert!(!Arc::ptr_eq(&first, &tab));
    }
}
