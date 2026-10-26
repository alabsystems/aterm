// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Viewport line projection, line indexing ([`EditorLines`]), and display-cell geometry.

use std::ops::Range;

use aterm_grapheme::{GraphemeClusters, grapheme_display_width};

use super::view::EditorBufferView;

pub(super) fn line_start(text: &str, position: usize) -> usize {
    text[..position.min(text.len())]
        .rfind('\n')
        .map_or(0, |at| at.saturating_add(1))
}

pub(super) fn line_end(text: &str, position: usize) -> usize {
    let position = position.min(text.len());
    text[position..]
        .find('\n')
        .map_or(text.len(), |relative| position.saturating_add(relative))
}

/// Where the text of `position`'s line ends: at its `\n`, or before the `\r` of a CRLF ending. amacs keeps CRLF byte-exact (design §2.2) and UAX #29 makes `\r\n` one character, so end-of-line motion, column motion and kill-line stop here and never park a caret between the two, where a typed character would split the ending.
pub(super) fn line_text_end(text: &str, position: usize) -> usize {
    let end = line_end(text, position);
    let before = end.saturating_sub(1);
    if end < text.len() && before >= position && text.as_bytes().get(before) == Some(&b'\r') {
        before
    } else {
        end
    }
}

/// The line ending a document's lines use, which a command that breaks a line puts in: `"\r\n"` when its first line ends in one, else `"\n"`. amacs keeps a CRLF file's endings byte-exact (design §2.2), so a newline it adds to one is a CRLF too.
pub(super) fn document_line_break(text: &str) -> &'static str {
    match text.find('\n') {
        Some(newline) if text[..newline].ends_with('\r') => "\r\n",
        _ => "\n",
    }
}

pub(super) fn clamp_to_char_boundary(text: &str, mut position: usize) -> usize {
    position = position.min(text.len());
    while position > 0 && !text.is_char_boundary(position) {
        position -= 1;
    }
    position
}

pub(super) fn byte_at_editor_display_column(
    text: &str,
    start: usize,
    end: usize,
    target: usize,
) -> usize {
    let mut column = 0usize;
    let mut byte = start;
    for (offset, grapheme) in text[start..end].grapheme_indices() {
        let width = editor_grapheme_columns(grapheme, column);
        if column.saturating_add(width) > target {
            break;
        }
        column = column.saturating_add(width);
        byte = start.saturating_add(offset).saturating_add(grapheme.len());
    }
    byte
}

pub(super) fn line_number(text: &str, position: usize) -> usize {
    text.as_bytes()[..position.min(text.len())]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count()
}

pub(super) fn byte_of_line(text: &str, line: usize) -> usize {
    if line == 0 {
        return 0;
    }
    text.as_bytes()
        .iter()
        .enumerate()
        .filter_map(|(index, byte)| (*byte == b'\n').then_some(index.saturating_add(1)))
        .nth(line - 1)
        .unwrap_or(text.len())
}

/// Byte offset at which every line of `text` begins.
#[cfg(any(test, debug_assertions))]
pub fn line_starts(text: &str) -> Vec<usize> {
    let mut starts = Vec::with_capacity(1 + text.len() / 40);
    starts.push(0);
    starts.extend(
        text.as_bytes()
            .iter()
            .enumerate()
            .filter_map(|(index, byte)| (*byte == b'\n').then_some(index.saturating_add(1))),
    );
    starts
}

/// Ceiling on resident line-index entries: 2 Mi × 8 B = **16 MiB**.
pub const MAX_LINE_INDEX_ENTRIES: usize = 2 << 20;

/// [`line_starts`], but declines rather than exceed [`MAX_LINE_INDEX_ENTRIES`].
pub fn line_starts_capped(text: &str) -> Option<Vec<usize>> {
    let mut starts = Vec::with_capacity((1 + text.len() / 40).min(MAX_LINE_INDEX_ENTRIES));
    starts.push(0);
    for (index, byte) in text.as_bytes().iter().enumerate() {
        if *byte == b'\n' {
            if starts.len() >= MAX_LINE_INDEX_ENTRIES {
                return None;
            }
            starts.push(index + 1);
        }
    }
    Some(starts)
}

/// Line count, line-of-byte, and byte-of-line queries over a document.
pub struct EditorLines<'a> {
    text: &'a str,
    starts: Option<&'a [usize]>,
}

impl<'a> EditorLines<'a> {
    /// Derive every answer from the bytes, allocation-free.
    pub const fn scanning(text: &'a str) -> Self {
        Self { text, starts: None }
    }

    /// Answer from a [`line_starts`] index built for exactly this `text`.
    pub const fn indexed(text: &'a str, starts: &'a [usize]) -> Self {
        Self {
            text,
            starts: Some(starts),
        }
    }

    pub const fn text(&self) -> &'a str {
        self.text
    }

    /// Total line count (an empty document is one line).
    pub fn total(&self) -> usize {
        self.starts.map_or_else(
            || {
                self.text
                    .bytes()
                    .filter(|byte| *byte == b'\n')
                    .count()
                    .saturating_add(1)
            },
            <[usize]>::len,
        )
    }

    /// The count of line breaks strictly before `position`.
    pub fn number_at(&self, position: usize) -> usize {
        self.starts.map_or_else(
            || line_number(self.text, position),
            |starts| {
                starts
                    .partition_point(|start| *start <= position)
                    .saturating_sub(1)
            },
        )
    }

    /// First byte of `line`; out-of-range lines clamp to EOF.
    pub fn byte_of(&self, line: usize) -> usize {
        self.starts.map_or_else(
            || byte_of_line(self.text, line),
            |starts| starts.get(line).copied().unwrap_or(self.text.len()),
        )
    }
}

/// A selected byte span within one projected editor line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorSelectionSpan {
    pub bytes: Range<usize>,
    pub continues: bool,
    pub primary: bool,
}

/// One bounded, source-addressable line prepared for native editor paint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorSyntaxClass {
    Table,
    Key,
    String,
    Number,
    Boolean,
    Comment,
}

/// One UTF-8-safe syntax run relative to [`EditorViewportLine::text`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorSyntaxSpan {
    pub bytes: Range<usize>,
    pub class: EditorSyntaxClass,
}

/// One UTF-8-safe diagnostic underline relative to the visible line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorDiagnosticSpan {
    pub bytes: Range<usize>,
    pub error: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorViewportLine {
    pub number: usize,
    pub source: Range<usize>,
    /// Display column of `source.start` in the unsliced logical line.
    pub column_start: usize,
    pub text: String,
    pub selections: Vec<EditorSelectionSpan>,
    pub carets: Vec<(usize, bool)>,
    pub syntax: Vec<EditorSyntaxSpan>,
    pub diagnostics: Vec<EditorDiagnosticSpan>,
}

/// Visible-only editor projection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorViewportProjection {
    pub first_line: usize,
    pub total_lines: usize,
    pub lines: Vec<EditorViewportLine>,
}

/// Project a stable, line-aligned editor viewport from canonical document bytes.
#[cfg(any(test, debug_assertions))]
pub fn project_viewport(
    text: &str,
    view: &EditorBufferView,
    requested_lines: usize,
    requested_columns: usize,
) -> EditorViewportProjection {
    project_viewport_with(
        &EditorLines::scanning(text),
        view,
        requested_lines,
        requested_columns,
    )
}

pub fn project_viewport_with(
    lines_index: &EditorLines<'_>,
    view: &EditorBufferView,
    requested_lines: usize,
    requested_columns: usize,
) -> EditorViewportProjection {
    const MAX_PROJECTED_LINES: usize = 512;
    const MAX_PROJECTED_LINE_BYTES: usize = 32 * 1024;
    const MAX_PROJECTED_TOTAL_BYTES: usize = 512 * 1024;

    let text = lines_index.text();
    let anchor = line_start(text, clamp_to_char_boundary(text, view.viewport_anchor));
    let first_line = lines_index.number_at(anchor);
    let total_lines = lines_index.total();
    let line_limit = requested_lines.clamp(1, MAX_PROJECTED_LINES);
    let column_limit = projected_column_limit(requested_columns);
    let primary_caret = clamp_to_char_boundary(text, view.primary_selection().head);
    let primary_line_start = line_start(text, primary_caret);
    let horizontal_anchor = horizontal_window_anchor(
        text,
        primary_line_start,
        primary_caret,
        MAX_PROJECTED_LINE_BYTES,
        column_limit,
    );
    let mut lines = Vec::with_capacity(line_limit.min(total_lines.saturating_sub(first_line)));
    let mut start = anchor;
    let mut number = first_line;
    let mut projected_bytes = 0usize;

    for _ in 0..line_limit {
        let full_end = line_end(text, start);
        let remaining = MAX_PROJECTED_TOTAL_BYTES.saturating_sub(projected_bytes);
        if remaining == 0 && !lines.is_empty() {
            break;
        }
        let budget = remaining.clamp(1, MAX_PROJECTED_LINE_BYTES);
        let display = bounded_line_window(
            text,
            start,
            full_end,
            horizontal_anchor,
            budget,
            column_limit,
        );
        let display_start = display.source.start;
        let display_end = display.source.end;
        let mut selections = Vec::new();
        let mut carets = Vec::new();
        for (index, selection) in view.selections.iter().enumerate() {
            let primary = index == view.primary;
            let range = selection.range();
            if !range.is_empty() && range.start <= display_end && range.end > display_start {
                let selected_start = range.start.max(display_start).min(display_end);
                let selected_end = range.end.min(display_end).max(selected_start);
                selections.push(EditorSelectionSpan {
                    bytes: selected_start.saturating_sub(display_start)
                        ..selected_end.saturating_sub(display_start),
                    continues: range.end > display_end,
                    primary,
                });
            }
            if selection.head >= display_start && selection.head <= display_end {
                carets.push((selection.head.saturating_sub(display_start), primary));
            }
        }
        let line_text = text[display.source.clone()].to_string();
        projected_bytes = projected_bytes.saturating_add(line_text.len());
        lines.push(EditorViewportLine {
            number,
            source: display.source,
            column_start: display.column_start,
            text: line_text,
            selections,
            carets,
            syntax: Vec::new(),
            diagnostics: Vec::new(),
        });

        if full_end == text.len() {
            break;
        }
        start = full_end.saturating_add(1);
        number = number.saturating_add(1);
    }

    EditorViewportProjection {
        first_line,
        total_lines,
        lines,
    }
}

trust_contract_fn! {
    /// How many display columns a projected line spans when the renderer
    /// offers `requested_columns`: at least 4 and at most 4096, the clamp the
    /// projection has always applied (`requested_columns.clamp(4, 4_096)`).
    #[expect(
        clippy::manual_clamp,
        reason = "Trust kernel-imports a select, not a call to `Ord::clamp` (E6 refuses calls)"
    )]
    fn projected_column_limit(requested_columns: usize) -> usize
    clauses [ensures result >= 4 && result <= 4096 by projected_column_limit_within_bounds
             ensures requested_columns < 4 || requested_columns > 4096 || result == requested_columns by projected_column_limit_keeps_an_in_range_value]
    {
        if requested_columns < 4 {
            4
        } else if requested_columns > 4_096 {
            4_096
        } else {
            requested_columns
        }
    }
}

verify_island! {
    clean {
        theorem projected_column_limit_floor_le_cap : Nat.le (UInt64.toNat 4) (UInt64.toNat 4096) := by
          decide

        theorem projected_column_limit_within_bounds (requested_columns : UInt64) :
            And (Nat.le (UInt64.toNat 4) (UInt64.toNat (trust_import_amacs__editor__projection__projected_column_limit requested_columns)))
                (Nat.le (UInt64.toNat (trust_import_amacs__editor__projection__projected_column_limit requested_columns)) (UInt64.toNat 4096)) :=
          @Decidable.casesOn (LT.lt requested_columns 4)
            (fun (d : Decidable (LT.lt requested_columns 4)) =>
              And (Nat.le (UInt64.toNat 4) (UInt64.toNat (@ite UInt64 (LT.lt requested_columns 4) d 4 (@ite UInt64 (LT.lt 4096 requested_columns) (instDecidableUInt64Lt 4096 requested_columns) 4096 requested_columns))))
                  (Nat.le (UInt64.toNat (@ite UInt64 (LT.lt requested_columns 4) d 4 (@ite UInt64 (LT.lt 4096 requested_columns) (instDecidableUInt64Lt 4096 requested_columns) 4096 requested_columns))) (UInt64.toNat 4096)))
            (instDecidableUInt64Lt requested_columns 4)
            (fun (at_least_floor : Not (LT.lt requested_columns 4)) =>
              @Decidable.casesOn (LT.lt 4096 requested_columns)
                (fun (e : Decidable (LT.lt 4096 requested_columns)) =>
                  And (Nat.le (UInt64.toNat 4) (UInt64.toNat (@ite UInt64 (LT.lt 4096 requested_columns) e 4096 requested_columns)))
                      (Nat.le (UInt64.toNat (@ite UInt64 (LT.lt 4096 requested_columns) e 4096 requested_columns)) (UInt64.toNat 4096)))
                (instDecidableUInt64Lt 4096 requested_columns)
                (fun (at_most_cap : Not (LT.lt 4096 requested_columns)) => And.intro (Nat.ge_of_not_lt at_least_floor) (Nat.ge_of_not_lt at_most_cap))
                (fun (_ : LT.lt 4096 requested_columns) => And.intro projected_column_limit_floor_le_cap (Nat.le_refl (UInt64.toNat 4096))))
            (fun (_ : LT.lt requested_columns 4) => And.intro (Nat.le_refl (UInt64.toNat 4)) projected_column_limit_floor_le_cap)

        theorem projected_column_limit_keeps_an_in_range_value (requested_columns : UInt64) :
            Or (Or (LT.lt requested_columns 4) (LT.lt 4096 requested_columns)) (Eq (trust_import_amacs__editor__projection__projected_column_limit requested_columns) requested_columns) :=
          @Decidable.casesOn (LT.lt requested_columns 4)
            (fun (d : Decidable (LT.lt requested_columns 4)) =>
              Or (Or (LT.lt requested_columns 4) (LT.lt 4096 requested_columns))
                 (Eq (@ite UInt64 (LT.lt requested_columns 4) d 4 (@ite UInt64 (LT.lt 4096 requested_columns) (instDecidableUInt64Lt 4096 requested_columns) 4096 requested_columns)) requested_columns))
            (instDecidableUInt64Lt requested_columns 4)
            (fun (_ : Not (LT.lt requested_columns 4)) =>
              @Decidable.casesOn (LT.lt 4096 requested_columns)
                (fun (e : Decidable (LT.lt 4096 requested_columns)) =>
                  Or (Or (LT.lt requested_columns 4) (LT.lt 4096 requested_columns)) (Eq (@ite UInt64 (LT.lt 4096 requested_columns) e 4096 requested_columns) requested_columns))
                (instDecidableUInt64Lt 4096 requested_columns)
                (fun (_ : Not (LT.lt 4096 requested_columns)) => Or.inr (Eq.refl requested_columns))
                (fun (above : LT.lt 4096 requested_columns) => Or.inl (Or.inr above)))
            (fun (below : LT.lt requested_columns 4) => Or.inl (Or.inl below))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HorizontalWindowAnchor {
    /// Exact display-column offset for ordinary lines. Every visible row uses
    /// the same offset, so vertical movement keeps columns visually aligned.
    Columns(usize),
    /// Bounded fallback for a pathological line whose prefix alone exceeds the
    /// per-line projection budget.
    ByteOffset {
        line_start: usize,
        window_start: usize,
    },
}

/// Display cells occupied by one complete editor grapheme at `column`.
pub fn editor_grapheme_columns(grapheme: &str, column: usize) -> usize {
    if grapheme == "\t" {
        4 - column % 4
    } else if grapheme.chars().any(char::is_control) {
        0
    } else {
        grapheme_display_width(grapheme)
    }
}

/// Display-cell offset of the complete graphemes ending at or before `byte`.
pub fn editor_display_column(text: &str, byte: usize, column_start: usize) -> usize {
    let requested = clamp_to_char_boundary(text, byte.min(text.len()));
    let mut column = column_start;
    for (offset, grapheme) in text.grapheme_indices() {
        let end = offset.saturating_add(grapheme.len());
        if end > requested {
            break;
        }
        column = column.saturating_add(editor_grapheme_columns(grapheme, column));
    }
    column.saturating_sub(column_start)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct HorizontalLineWindow {
    source: Range<usize>,
    column_start: usize,
}

fn horizontal_window_anchor(
    text: &str,
    line_start: usize,
    caret: usize,
    byte_budget: usize,
    column_budget: usize,
) -> HorizontalWindowAnchor {
    let left_target = column_budget.saturating_mul(3) / 4;
    if caret.saturating_sub(line_start) <= byte_budget {
        // The caret's column, then how far its line runs past it (scanned no further than one
        // window: past that the line's end never holds the window back).
        let mut columns = 0usize;
        let mut line_columns = 0usize;
        let end = line_end(text, caret);
        for (offset, grapheme) in text[line_start..end].grapheme_indices() {
            let before_caret = line_start
                .saturating_add(offset)
                .saturating_add(grapheme.len())
                <= caret;
            if !before_caret && line_columns >= columns.saturating_add(column_budget) {
                break;
            }
            line_columns =
                line_columns.saturating_add(editor_grapheme_columns(grapheme, line_columns));
            if before_caret {
                columns = line_columns;
            }
        }
        // The caret sits three quarters of the way across, but the window never scrolls further
        // than the caret's line needs: where its end and the caret after it fit, the window
        // stops there, so a line that fits whole shows from its first column (a caret at column
        // 26 of a 29-column window cut the first four columns off every row of aterm.toml).
        let needed = line_columns.saturating_add(1).saturating_sub(column_budget);
        return HorizontalWindowAnchor::Columns(columns.saturating_sub(left_target).min(needed));
    }

    let search_start =
        clamp_to_char_boundary(text, caret.saturating_sub(byte_budget).max(line_start));
    let mut window_start = caret;
    let mut left_columns = 0usize;
    let graphemes = text[search_start..caret]
        .grapheme_indices()
        .collect::<Vec<_>>();
    for (relative, grapheme) in graphemes.into_iter().rev() {
        if relative == 0 && search_start > line_start {
            break;
        }
        let columns = if grapheme == "\t" {
            4
        } else {
            editor_grapheme_columns(grapheme, 0)
        };
        if left_columns.saturating_add(columns) > left_target {
            break;
        }
        window_start = search_start.saturating_add(relative);
        left_columns = left_columns.saturating_add(columns);
    }
    HorizontalWindowAnchor::ByteOffset {
        line_start,
        window_start,
    }
}

fn bounded_line_window(
    text: &str,
    start: usize,
    end: usize,
    anchor: HorizontalWindowAnchor,
    byte_budget: usize,
    column_budget: usize,
) -> HorizontalLineWindow {
    let byte_budget = byte_budget.max(1);
    let column_budget = column_budget.max(1);
    let (window_start, column_start) = match anchor {
        HorizontalWindowAnchor::Columns(target) => {
            let mut at = start;
            let mut columns = 0usize;
            for (relative, grapheme) in text[start..end].grapheme_indices() {
                if columns >= target {
                    break;
                }
                let grapheme_columns = editor_grapheme_columns(grapheme, columns);
                if columns.saturating_add(grapheme_columns) > target {
                    break;
                }
                columns = columns.saturating_add(grapheme_columns);
                at = start
                    .saturating_add(relative)
                    .saturating_add(grapheme.len());
            }
            (at, columns)
        }
        HorizontalWindowAnchor::ByteOffset {
            line_start,
            window_start,
        } => {
            // Only the pathological primary line owns a proven bounded grapheme start.
            if start == line_start {
                (window_start.clamp(start, end), 0)
            } else {
                (start, 0)
            }
        }
    };

    let mut window_end = window_start;
    let mut columns = column_start;
    for (relative, grapheme) in text[window_start..end].grapheme_indices() {
        let next = window_start
            .saturating_add(relative)
            .saturating_add(grapheme.len());
        if next.saturating_sub(window_start) > byte_budget {
            break;
        }
        let grapheme_columns = editor_grapheme_columns(grapheme, columns);
        if columns
            .saturating_add(grapheme_columns)
            .saturating_sub(column_start)
            > column_budget
            && window_end > window_start
        {
            break;
        }
        columns = columns.saturating_add(grapheme_columns);
        window_end = next;
        if columns.saturating_sub(column_start) >= column_budget {
            break;
        }
    }
    if window_end == window_start && window_start < end {
        window_end = text[window_start..end]
            .graphemes()
            .next()
            .filter(|grapheme| grapheme.len() <= byte_budget)
            .map_or(window_start, |grapheme| {
                window_start.saturating_add(grapheme.len()).min(end)
            });
    }
    HorizontalLineWindow {
        source: window_start..window_end,
        column_start,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{EditorSelectionSpan, Selection, editor};

    #[test]
    fn projected_column_limit_is_the_clamp_it_replaced() {
        for columns in [0, 3, 4, 5, 80, 4_095, 4_096, 4_097, usize::MAX] {
            assert_eq!(projected_column_limit(columns), columns.clamp(4, 4_096));
        }
    }

    #[test]
    fn indexed_line_oracle_matches_the_scanning_one() {
        for text in [
            "",
            "\n",
            "one",
            "one\n",
            "one\ntwo",
            "one\ntwo\n",
            "\n\n\n",
            "α\nβγ\n\nδ",
        ] {
            let starts = line_starts(text);
            let scanning = EditorLines::scanning(text);
            let indexed = EditorLines::indexed(text, &starts);
            assert_eq!(scanning.total(), indexed.total(), "total for {text:?}");
            for position in 0..=text.len() + 2 {
                assert_eq!(
                    scanning.number_at(position),
                    indexed.number_at(position),
                    "number_at({position}) for {text:?}"
                );
            }
            for line in 0..scanning.total() + 2 {
                assert_eq!(
                    scanning.byte_of(line),
                    indexed.byte_of(line),
                    "byte_of({line}) for {text:?}"
                );
            }
        }
    }

    #[test]
    fn editor_cell_widths_share_tab_unicode_and_suppressed_control_semantics() {
        assert_eq!(editor_grapheme_columns("\t", 0), 4);
        assert_eq!(editor_grapheme_columns("\t", 3), 1);
        assert_eq!(editor_grapheme_columns("e\u{301}", 0), 1);
        assert_eq!(editor_grapheme_columns("中", 0), 2);
        assert_eq!(editor_grapheme_columns("👩‍💻", 0), 2);
        assert_eq!(editor_grapheme_columns("\u{0001}", 17), 0);
    }

    #[test]
    fn viewport_projection_is_visible_only_utf8_safe_and_source_addressable() {
        let text = "zero\nβeta\nlast\n";
        let (_, _, mut view, _) = editor(text);
        view.viewport_anchor = "zero\n".len() + 1;
        view.selections = vec![
            Selection {
                anchor: "zero\n".len(),
                head: "zero\nβeta\nlast".len(),
            },
            Selection::caret("zero\nβ".len()),
        ];
        view.primary = 0;

        let projection = project_viewport(text, &view, 2, usize::MAX);
        assert_eq!(projection.first_line, 1);
        assert_eq!(projection.total_lines, 4);
        assert_eq!(projection.lines.len(), 2, "projection stays visible-only");
        assert_eq!(projection.lines[0].number, 1);
        assert_eq!(
            projection.lines[0].source,
            "zero\n".len().."zero\nβeta".len()
        );
        assert_eq!(projection.lines[0].text, "βeta");
        assert_eq!(projection.lines[0].carets, vec![("β".len(), false)]);
        assert_eq!(
            projection.lines[0].selections,
            vec![EditorSelectionSpan {
                bytes: 0.."βeta".len(),
                continues: true,
                primary: true,
            }]
        );
        assert_eq!(projection.lines[1].text, "last");
    }

    #[test]
    fn viewport_projection_keeps_trailing_empty_line_and_selected_newline_cell() {
        let text = "a\n";
        let (_, _, mut view, _) = editor(text);
        view.selections = vec![Selection { anchor: 0, head: 2 }];

        let projection = project_viewport(text, &view, usize::MAX, usize::MAX);
        assert_eq!(projection.total_lines, 2);
        assert_eq!(projection.lines.len(), 2);
        assert_eq!(projection.lines[0].text, "a");
        assert_eq!(projection.lines[0].selections[0].bytes, 0..1);
        assert!(projection.lines[0].selections[0].continues);
        assert_eq!(projection.lines[1].source, 2..2);
        assert_eq!(projection.lines[1].text, "");
        assert_eq!(projection.lines[1].carets, vec![(0, true)]);
    }

    #[test]
    fn compact_horizontal_window_follows_caret_and_is_shared_across_rows() {
        let line = "0123456789".repeat(24);
        let text = format!("{line}\n{line}\n{line}");
        let (_, _, mut view, _) = editor(&text);
        let caret = 173;
        view.selections = vec![Selection::caret(caret)];

        let projection = project_viewport(&text, &view, 3, 40);
        assert_eq!(projection.lines.len(), 3);
        let expected_start = caret - 30;
        for (row, projected) in projection.lines.iter().enumerate() {
            let line_start = row * (line.len() + 1);
            assert_eq!(projected.source.start, line_start + expected_start);
            assert_eq!(projected.column_start, expected_start);
            assert!(projected.text.chars().count() <= 40);
            assert!(text.is_char_boundary(projected.source.start));
            assert!(text.is_char_boundary(projected.source.end));
        }
        assert_eq!(projection.lines[0].carets, vec![(30, true)]);
        assert!(
            projection.lines[0].source.start > 0,
            "negative control: the old projection always began ordinary lines at byte zero"
        );
    }

    /// THE WINDOW SCROLLS NO FURTHER THAN THE CARET'S LINE NEEDS: the window put the caret three
    /// quarters of the way across whatever the line held, so a caret at the end of
    /// `agents_auto_prime = false` (column 25) in a 29-column window started every row at column
    /// 4 ("ts_auto_prime", "ate]", "led = false") although the line fitted whole. Where the
    /// caret's line ends (the caret after it) inside the window it stops there: a line that fits
    /// shows from its first column, and a long line's end lands at the right edge.
    #[test]
    fn the_horizontal_window_scrolls_no_further_than_the_caret_line_needs() {
        let text = "agents_auto_prime = false\n[update]\nenabled = false\n";
        let (_, _, mut view, _) = editor(text);
        view.selections = vec![Selection::caret(25)];
        let projection = project_viewport(text, &view, 3, 29);
        let texts: Vec<&str> = projection
            .lines
            .iter()
            .map(|line| line.text.as_str())
            .collect();
        assert_eq!(
            texts,
            ["agents_auto_prime = false", "[update]", "enabled = false"]
        );
        assert!(projection.lines.iter().all(|line| line.column_start == 0));
        assert_eq!(projection.lines[0].carets, vec![(25, true)]);

        // A line one column too wide for the caret after it scrolls one column, not to 3/4.
        let view_at = |caret: usize| {
            let (_, _, mut view, _) = editor(text);
            view.selections = vec![Selection::caret(caret)];
            view
        };
        let projection = project_viewport(text, &view_at(25), 3, 25);
        assert_eq!(projection.lines[0].column_start, 1);
        assert_eq!(projection.lines[0].text, "gents_auto_prime = false");

        // In a long line the caret still keeps a quarter of the window ahead of it, until the
        // line's end comes into view.
        let long = format!("{}\n", "0123456789".repeat(10));
        let (_, _, mut view, _) = editor(&long);
        for (caret, start) in [(50, 20), (80, 50), (85, 55), (91, 61), (100, 61)] {
            view.selections = vec![Selection::caret(caret)];
            let projection = project_viewport(&long, &view, 1, 40);
            assert_eq!(
                projection.lines[0].column_start, start,
                "caret at column {caret}"
            );
            let carets = &projection.lines[0].carets;
            assert_eq!(
                carets,
                &vec![(caret - start, true)],
                "caret at column {caret}"
            );
        }
    }

    #[test]
    fn compact_horizontal_window_is_utf8_and_tab_safe() {
        let line = format!("{}\t{}", "α".repeat(80), "終".repeat(80));
        let text = format!("{line}\n{line}");
        let (_, _, mut view, _) = editor(&text);
        let caret = "α".len() * 70;
        view.selections = vec![Selection::caret(caret)];

        let projection = project_viewport(&text, &view, 2, 32);
        assert_eq!(projection.lines.len(), 2);
        assert!(projection.lines[0].source.start > 0);
        assert_eq!(projection.lines[0].column_start, 46);
        assert_eq!(projection.lines[1].column_start, 46);
        assert!(
            projection.lines[0]
                .carets
                .iter()
                .any(|(byte, primary)| { *primary && *byte <= projection.lines[0].text.len() })
        );
        for line in &projection.lines {
            assert!(text.is_char_boundary(line.source.start));
            assert!(text.is_char_boundary(line.source.end));
        }
    }

    #[test]
    fn compact_horizontal_window_never_splits_zwj_or_combining_graphemes() {
        let text = format!("{}👩‍💻x", "a".repeat(31));
        let (_, _, mut view, _) = editor(&text);
        view.selections = vec![Selection::caret(text.len())];

        let projection = project_viewport(&text, &view, 1, 4);
        let line = &projection.lines[0];
        assert_eq!(line.text, "👩‍💻x");
        assert_eq!(line.source.start, 31);
        assert!(
            line.source.start == text.len()
                || text
                    .grapheme_indices()
                    .any(|(boundary, _)| boundary == line.source.start)
        );
        assert!(
            line.source.end == text.len()
                || text
                    .grapheme_indices()
                    .any(|(boundary, _)| boundary == line.source.end)
        );

        let text = "abce\u{301}z";
        let (_, _, view, _) = editor(text);
        let projection = project_viewport(text, &view, 1, 4);
        let line = &projection.lines[0];
        assert_eq!(line.text, "abce\u{301}");
        assert_eq!(line.source, 0.."abce\u{301}".len());
        assert_eq!(
            line.text.graphemes().collect::<Vec<_>>(),
            ["a", "b", "c", "e\u{301}"]
        );
    }

    #[test]
    fn viewport_projection_bounds_pathological_long_line_and_keeps_caret_visible() {
        let text = "α".repeat(600_000);
        let (_, _, mut view, _) = editor(&text);
        let caret = text.len() - "α".len() * 9;
        view.selections = vec![Selection::caret(caret)];

        let projection = project_viewport(&text, &view, usize::MAX, usize::MAX);
        assert_eq!(projection.lines.len(), 1);
        let line = &projection.lines[0];
        assert!(line.text.len() <= 32 * 1024);
        assert!(line.source.start <= caret && caret <= line.source.end);
        assert_eq!(line.carets, vec![(caret - line.source.start, true)]);
        assert!(text.is_char_boundary(line.source.start));
        assert!(text.is_char_boundary(line.source.end));
    }

    #[test]
    fn viewport_projection_drops_an_over_budget_grapheme_instead_of_splitting_it() {
        let text = format!("e{}", "\u{301}".repeat(20_000));
        assert!(text.len() > 32 * 1024);
        assert_eq!(text.graphemes().count(), 1);
        let (_, _, view, _) = editor(&text);

        let projection = project_viewport(&text, &view, 1, usize::MAX);
        let line = &projection.lines[0];
        assert_eq!(line.text, "");
        assert_eq!(line.source, 0..0);
        assert!(line.text.len() <= 32 * 1024);
    }

    #[test]
    fn viewport_projection_has_a_total_byte_budget_across_many_large_lines() {
        let line = format!("{}\n", "\u{0001}".repeat(40 * 1024));
        let text = line.repeat(40);
        let (_, _, view, _) = editor(&text);
        let projection = project_viewport(&text, &view, usize::MAX, usize::MAX);
        let total = projection
            .lines
            .iter()
            .map(|line| line.text.len())
            .sum::<usize>();
        assert!(total <= 512 * 1024);
        assert!(
            projection.lines.len() < 40,
            "the byte budget truncates rows"
        );
    }

    #[test]
    fn line_index_is_capped_and_the_fallback_answers_identically() {
        let dense = "\n".repeat(MAX_LINE_INDEX_ENTRIES + 8);
        assert!(
            line_starts_capped(&dense).is_none(),
            "a document past the cap must decline to be indexed"
        );

        let ok = "\n".repeat(64);
        let starts = line_starts_capped(&ok).expect("under the cap must index");
        assert_eq!(
            starts,
            line_starts(&ok),
            "capped index must equal the plain one"
        );

        let scanning = EditorLines::scanning(&dense);
        let reference = line_starts(&dense);
        let indexed = EditorLines::indexed(&dense, &reference);
        assert_eq!(scanning.total(), indexed.total());
        for position in [0, 1, 7, dense.len() / 2, dense.len(), dense.len() + 3] {
            assert_eq!(
                scanning.number_at(position),
                indexed.number_at(position),
                "number_at({position}) must not depend on whether an index exists"
            );
        }
    }
}
