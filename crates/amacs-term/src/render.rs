// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The terminal face's frame (design §5.4): what one [`Screen`] of the
//! terminal UI shows, built from the engine's state and nothing else.
//!
//! The layout is Emacs's in a terminal: the buffer, then one reverse-video
//! modeline, then the echo area on the last row, which is the minibuffer
//! while one is open. A message or a prompt wider than the screen wraps, and
//! the echo area grows upward to hold it, to a quarter of the screen (Emacs's
//! `max-mini-window-height`), taking rows from the buffer while it is shown;
//! M-x's completions stay on one row and are cut at its end. The buffer rows
//! are the engine's own
//! projection ([`project_viewport_with`]): the same lines, horizontal window
//! and selections aterm's native Editor paints, laid out a grapheme at a time
//! on the screen writer, which places each cell where aterm's grid puts it. A
//! tab is the blanks to the next four-column stop and a control character
//! takes no cell, the engine's own column rules
//! ([`amacs::editor::editor_grapheme_columns`]), so a caret's column here is
//! the column `C-n` and `C-p` keep. Each drawn row remembers where its
//! graphemes landed, which is how a mouse press finds its document byte.
//!
//! Colours are the 16 named ones by role (keyword, string, comment, ...), so
//! the terminal's own theme applies, aterm's when it is the terminal; the
//! region and the isearch match are reverse video. The cursor is a block in
//! the buffer and a bar in the minibuffer.

use std::ops::Range;

use amacs::editor::{
    EditorBufferView, EditorLines, EditorViewportLine, Minibuffer, command_completions,
    editor_display_column, editor_grapheme_columns, project_viewport_with,
};
use amacs::language::{Language, LexCache, SyntaxClass};
use aterm_grapheme::GraphemeClusters as _;
use aterm_tty::screen::{Attrs, Color, CursorShape, Pos, Screen, Style};
use aterm_tty::tty::Size;
use aterm_types::text_shaping::AmbiguousWidth;

/// Where the three parts of the layout sit on a screen of some size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Layout {
    /// How many rows, from the top, show the buffer.
    pub body: u16,
    /// The modeline's row, when the screen has room for one.
    pub modeline: Option<u16>,
    /// The echo area's rows, the last of the screen.
    pub echo: Range<u16>,
}

/// The rows of a screen `rows` tall whose echo area needs `echo` rows (at
/// least one): the echo area takes the last, the modeline the one above
/// them, and the buffer the rest. A screen too short for all three keeps the
/// echo area first, then the modeline.
pub(crate) fn layout(rows: u16, echo: u16) -> Layout {
    let echo = echo.clamp(1, rows.max(1)).min(rows);
    let above = rows.saturating_sub(echo);
    let modeline = above.checked_sub(1);
    Layout {
        body: modeline.unwrap_or(0),
        modeline,
        echo: above..rows,
    }
}

/// The most rows the echo area grows to on a screen `rows` tall: a quarter
/// of it, and always one.
pub(crate) fn echo_limit(rows: u16) -> u16 {
    (rows / 4).max(1)
}

/// One buffer row as drawn: which document bytes it shows, and the column
/// each of its graphemes starts at.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct DrawnRow {
    /// The document bytes the row shows ([`EditorViewportLine::source`]).
    pub source: Range<usize>,
    /// Each grapheme's offset in `source` and the screen column it starts at,
    /// in order. A control character, which takes no cell, starts where the
    /// grapheme after it does.
    pub cells: Vec<(usize, u16)>,
    /// The column after the row's last drawn grapheme.
    pub end: u16,
}

impl DrawnRow {
    /// The column a caret at `offset` into `source` shows at: the column of
    /// the grapheme it stands before, or the row's end.
    fn column_of(&self, offset: usize) -> u16 {
        self.cells
            .iter()
            .find(|(at, _)| *at >= offset)
            .map_or(self.end, |(_, column)| *column)
    }

    /// The document byte a press at `column` lands on: the start of the
    /// grapheme drawn there (the left one of a wide pair), or the row's end
    /// for a press past its text.
    pub(crate) fn byte_at(&self, column: u16) -> usize {
        let offset = self
            .cells
            .iter()
            .take_while(|(_, start)| *start <= column)
            .last()
            .filter(|_| column < self.end)
            .map_or(self.source.len(), |(at, _)| *at);
        self.source.start.saturating_add(offset)
    }
}

/// The syntax of a document as the frame needs it: its language's spans,
/// line by line, and the text they were lexed from.
pub(crate) struct Syntax<'a> {
    pub cache: &'a LexCache,
    pub lines: &'a EditorLines<'a>,
}

/// The style a span of `class` is drawn in: a named colour by role, so the
/// terminal's own theme decides the shade.
pub(crate) fn class_style(class: SyntaxClass) -> Style {
    let color = |index: u8| Style {
        fg: Color::Indexed(index),
        ..Style::default()
    };
    match class {
        SyntaxClass::Keyword => color(5),
        SyntaxClass::String | SyntaxClass::Added => color(2),
        SyntaxClass::Comment => color(6),
        SyntaxClass::Number => color(3),
        SyntaxClass::Heading => Style {
            attrs: Attrs::BOLD,
            ..color(4)
        },
        SyntaxClass::Punct => Style::default(),
        SyntaxClass::Removed => color(1),
        SyntaxClass::Meta => color(4),
    }
}

/// `style` drawn in reverse video: the region, an isearch match, a caret
/// that is not the terminal's own.
const fn reversed(style: Style) -> Style {
    Style {
        attrs: style.attrs.union(Attrs::REVERSE),
        ..style
    }
}

/// The blank cells a tab at display column `column` takes: up to the next
/// stop of four, as the engine counts it.
const fn tab_cells(column: usize) -> u16 {
    match column % 4 {
        0 => 4,
        1 => 3,
        2 => 2,
        _ => 1,
    }
}

/// Draw the buffer rows of `view` over `text` on `screen`, `rows` of them
/// from the top, and return each drawn row and where the primary caret
/// shows (`None` when it is off the rows drawn).
pub(crate) fn draw_body(
    screen: &mut Screen,
    lines: &EditorLines<'_>,
    view: &EditorBufferView,
    syntax: Option<&Syntax<'_>>,
    rows: u16,
) -> (Vec<DrawnRow>, Option<Pos>) {
    let cols = screen.size().cols;
    let projection = project_viewport_with(lines, view, usize::from(rows), usize::from(cols));
    let mut drawn = Vec::new();
    let mut caret = None;
    for (row, line) in (0..rows).zip(projection.lines.iter()) {
        let spans = syntax.map_or(&[][..], |syntax| syntax.cache.spans(line.number));
        // Spans count from the line's first byte; the row may start further
        // in, when the caret's line is scrolled sideways.
        let shift = syntax.map_or(0, |syntax| {
            line.source
                .start
                .saturating_sub(syntax.lines.byte_of(line.number))
        });
        let row_drawn = draw_line(screen, row, line, spans, shift);
        for (offset, primary) in &line.carets {
            let column = row_drawn.column_of(*offset);
            if *primary {
                caret = Some(Pos {
                    row,
                    col: column.min(cols.saturating_sub(1)),
                });
            } else if column < cols {
                // A caret that is not the terminal's own is a reversed cell.
                let to = column.saturating_add(1);
                let style = screen
                    .cell(row, column)
                    .map(|cell| cell.style())
                    .unwrap_or_default();
                screen.set_style(row, column, to, reversed(style));
            }
        }
        drawn.push(row_drawn);
    }
    (drawn, caret)
}

/// Lay one projected line out on `row`.
fn draw_line(
    screen: &mut Screen,
    row: u16,
    line: &EditorViewportLine,
    spans: &[amacs::language::Span],
    shift: usize,
) -> DrawnRow {
    let cols = screen.size().cols;
    let mut drawn = DrawnRow {
        source: line.source.clone(),
        ..DrawnRow::default()
    };
    let mut column = 0u16;
    for (offset, grapheme) in line.text.grapheme_indices() {
        if column >= cols {
            break;
        }
        drawn.cells.push((offset, column));
        let mut style = spans
            .iter()
            .find(|span| span.range.contains(&offset.saturating_add(shift)))
            .map_or_else(Style::default, |span| class_style(span.class));
        if line
            .selections
            .iter()
            .any(|selection| selection.bytes.contains(&offset))
        {
            style = reversed(style);
        }
        if grapheme == "\t" {
            let from = usize::from(column).saturating_add(line.column_start);
            let to = column.saturating_add(tab_cells(from)).min(cols);
            screen.fill(row, column, to, style);
            column = to;
        } else if grapheme.chars().any(char::is_control) {
            // The engine gives a control character no column; neither does
            // the frame, so the caret's column is the one `C-n` keeps.
        } else {
            let next = screen.put(row, column, grapheme, style);
            if next == column && editor_grapheme_columns(grapheme, usize::from(column)) > 0 {
                // It does not fit: a wide character at the last column.
                drawn.cells.pop();
                break;
            }
            column = next;
        }
    }
    drawn.end = column;
    // A selection that owns the line break paints the cell after the text.
    if column < cols
        && line
            .selections
            .iter()
            .any(|selection| selection.continues && selection.bytes.end >= line.text.len())
    {
        screen.fill(
            row,
            column,
            column.saturating_add(1),
            reversed(Style::default()),
        );
    }
    drawn
}

/// What the modeline says about the buffer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Modeline<'a> {
    pub name: &'a str,
    pub dirty: bool,
    pub language: Language,
    /// The caret's line, 1-based.
    pub line: usize,
    /// The caret's display column, 0-based, as Emacs counts it.
    pub column: usize,
    /// The program the edit is for (`for=`), named in the edit's hint.
    pub for_word: Option<&'a str>,
}

/// The modeline's text: `-UU-:**- name  (Language) L12 C4`, then, when the
/// edit is for a program, how to finish or abandon it.
pub(crate) fn modeline_text(modeline: &Modeline<'_>) -> String {
    let flag = if modeline.dirty { "**" } else { "--" };
    let mut text = format!(
        "-UU-:{flag}- {}  ({}) L{} C{}",
        modeline.name,
        modeline.language.label(),
        modeline.line,
        modeline.column
    );
    if let Some(word) = modeline.for_word {
        text.push_str(&format!("  [for {word} · C-c C-c done · C-c C-k abort]"));
    }
    text
}

/// Draw the modeline on `row`, reverse video across the whole row.
pub(crate) fn draw_modeline(screen: &mut Screen, row: u16, modeline: &Modeline<'_>) {
    let style = reversed(Style::default());
    let cols = screen.size().cols;
    screen.fill(row, 0, cols, style);
    screen.put(row, 0, &modeline_text(modeline), style);
}

/// The caret's line (1-based) and display column (0-based) in `text`.
pub(crate) fn caret_line_column(
    lines: &EditorLines<'_>,
    view: &EditorBufferView,
) -> (usize, usize) {
    let text = lines.text();
    let caret = view.primary_selection().head.min(text.len());
    let number = lines.number_at(caret);
    let start = lines.byte_of(number).min(caret);
    let line_text = text.get(start..).unwrap_or("");
    let end = line_text.find('\n').unwrap_or(line_text.len());
    let column = editor_display_column(
        line_text.get(..end).unwrap_or(""),
        caret.saturating_sub(start),
        0,
    );
    (number.saturating_add(1), column)
}

/// What the echo area shows, most pressing first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Echo<'a> {
    /// A question of the face's own (`Save FILE? (y, n, q)`), the cursor
    /// after it.
    Question(&'a str),
    /// The engine's echo of a key sequence being typed (`C-x — …`).
    Hud(&'a str),
    /// An open minibuffer, its query typed after its label.
    Minibuffer {
        minibuffer: &'a Minibuffer,
        /// The status line the query's last key left: isearch says
        /// "Failing I-search" there.
        status: Option<&'a str>,
    },
    /// A message (`Wrote …`, `Quit`).
    Message(&'a str),
    Empty,
}

/// The echo area's content, laid out: its text, M-x's completions after it,
/// and whether the cursor stands at the text's end (a question or a
/// minibuffer takes the keys).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct EchoLine {
    pub text: String,
    /// The commands Enter may run, the one it runs first.
    pub completions: Vec<String>,
    pub prompt: bool,
}

impl EchoLine {
    /// The echo area for `echo`. A line break, which a paste into the
    /// minibuffer may hold, shows as a space.
    pub(crate) fn of(echo: &Echo<'_>) -> Self {
        let one_line = |text: &str| text.replace(['\r', '\n'], " ");
        match echo {
            Echo::Empty => Self::default(),
            Echo::Hud(text) | Echo::Message(text) => Self {
                text: one_line(text),
                ..Self::default()
            },
            Echo::Question(text) => Self {
                text: one_line(text),
                prompt: true,
                ..Self::default()
            },
            Echo::Minibuffer { minibuffer, status } => {
                let (text, completions) = minibuffer_line(minibuffer, *status);
                Self {
                    text: one_line(&text),
                    completions,
                    prompt: true,
                }
            }
        }
    }
}

/// The echo area's label and query for an open minibuffer, and the
/// completions M-x offers after it (the chosen one first).
fn minibuffer_line(minibuffer: &Minibuffer, status: Option<&str>) -> (String, Vec<String>) {
    let (label, query) = match minibuffer {
        Minibuffer::Inactive => return (String::new(), Vec::new()),
        Minibuffer::Command { query, .. } => ("M-x ", query),
        Minibuffer::Search { query, .. } => (
            if status.is_some_and(|status| status.starts_with("Failing")) {
                "Failing I-search: "
            } else {
                "I-search: "
            },
            query,
        ),
        Minibuffer::Buffer { query } => ("Switch to buffer: ", query),
        Minibuffer::GotoLine { query, .. } => ("Goto line: ", query),
        Minibuffer::Prompt { query, .. } => (minibuffer.prompt_label().unwrap_or(""), query),
    };
    let completions = match minibuffer {
        Minibuffer::Command { query, selected } => command_completions(query)
            .iter()
            .skip(*selected)
            .map(|command| command.name().to_string())
            .collect(),
        _ => Vec::new(),
    };
    (format!("{label}{query}"), completions)
}

/// Lay `echo` out on `rows` of `screen`: the text wraps from row to row and
/// stops at the last; the completions follow it on its last row and are cut
/// at that row's end. Returns how many rows the text took and where the
/// cursor stands after it, for a prompt.
fn lay_echo(screen: &mut Screen, rows: Range<u16>, echo: &EchoLine) -> (u16, Option<Pos>) {
    let plain = Style::default();
    let cols = screen.size().cols;
    if rows.is_empty() || cols == 0 {
        return (0, None);
    }
    let (mut row, mut col) = (rows.start, 0u16);
    for grapheme in echo.text.graphemes() {
        let next = screen.put(row, col, grapheme, plain);
        let takes_cells = editor_grapheme_columns(grapheme, usize::from(col)) > 0;
        if next != col || !takes_cells {
            col = next;
            continue;
        }
        // It does not fit on this row: the next takes it, if there is one.
        if col == 0 || row.saturating_add(1) >= rows.end {
            break;
        }
        row = row.saturating_add(1);
        col = screen.put(row, 0, grapheme, plain);
    }
    let cursor = echo.prompt.then(|| Pos {
        row,
        col: col.min(cols.saturating_sub(1)),
    });
    if !echo.completions.is_empty() {
        let mut at = screen.put(row, col, "  {", plain);
        for (index, name) in echo.completions.iter().enumerate() {
            if index > 0 {
                at = screen.put(row, at, " | ", plain);
            }
            // The one Enter runs stands out.
            let style = if index == 0 { reversed(plain) } else { plain };
            at = screen.put(row, at, name, style);
        }
        screen.put(row, at, "}", plain);
    }
    (row.saturating_sub(rows.start).saturating_add(1), cursor)
}

/// How many rows the echo area needs for `echo` on a screen of `size`: one,
/// or as many as its text wraps to, up to [`echo_limit`].
pub(crate) fn echo_rows(size: Size, ambiguous: AmbiguousWidth, echo: &EchoLine) -> u16 {
    let limit = echo_limit(size.rows);
    let mut scratch = Screen::new(
        Size {
            cols: size.cols,
            rows: limit,
        },
        ambiguous,
    );
    lay_echo(&mut scratch, 0..limit, echo).0.max(1)
}

/// Everything one frame shows.
pub(crate) struct FrameParts<'a> {
    pub lines: &'a EditorLines<'a>,
    pub view: &'a EditorBufferView,
    pub syntax: Option<Syntax<'a>>,
    pub modeline: Modeline<'a>,
    pub echo: &'a EchoLine,
}

/// Build the frame `parts` describe on a screen of `size`, laid out as
/// `layout` says.
pub(crate) fn frame(
    size: Size,
    ambiguous: AmbiguousWidth,
    layout: &Layout,
    parts: &FrameParts<'_>,
) -> (Screen, Vec<DrawnRow>) {
    let mut screen = Screen::new(size, ambiguous);
    let (rows, caret) = draw_body(
        &mut screen,
        parts.lines,
        parts.view,
        parts.syntax.as_ref(),
        layout.body,
    );
    if let Some(row) = layout.modeline {
        draw_modeline(&mut screen, row, &parts.modeline);
    }
    let (_, prompt) = lay_echo(&mut screen, layout.echo.clone(), parts.echo);
    // The cursor is where the keys go: the echo area while it asks, the
    // buffer's caret otherwise.
    let (cursor, shape) = match prompt {
        Some(pos) => (Some(pos), CursorShape::SteadyBar),
        None => (caret, CursorShape::SteadyBlock),
    };
    screen.set_cursor(cursor);
    screen.set_cursor_shape(shape);
    (screen, rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every screen keeps its echo area, then its modeline, and gives the
    /// buffer the rest; a taller echo area takes rows from the buffer.
    #[test]
    fn the_layout_keeps_the_echo_area_then_the_modeline() {
        let at = |body, modeline, echo| Layout {
            body,
            modeline,
            echo,
        };
        assert_eq!(layout(0, 1), at(0, None, 0..0));
        assert_eq!(layout(1, 1), at(0, None, 0..1));
        assert_eq!(layout(2, 1), at(0, Some(0), 1..2));
        assert_eq!(layout(24, 0), at(22, Some(22), 23..24));
        assert_eq!(layout(24, 1), at(22, Some(22), 23..24));
        assert_eq!(layout(24, 3), at(20, Some(20), 21..24));
        assert_eq!(layout(3, 9), at(0, None, 0..3));
        assert_eq!(echo_limit(24), 6);
        assert_eq!(echo_limit(3), 1);
    }

    /// A message wider than the screen wraps onto rows above the last, up
    /// to the limit; M-x's completions are cut at the row's end.
    #[test]
    fn a_long_message_wraps_and_completions_are_cut() {
        let size = Size { cols: 10, rows: 8 };
        let echo = EchoLine {
            text: "0123456789abcdefghij-tail".to_string(),
            ..EchoLine::default()
        };
        assert_eq!(
            echo_rows(size, AmbiguousWidth::Single, &echo),
            2,
            "the limit"
        );
        let mut screen = Screen::new(size, AmbiguousWidth::Single);
        let (rows, cursor) = lay_echo(&mut screen, 5..8, &echo);
        assert_eq!((rows, cursor), (3, None));
        assert_eq!(screen.row_text(5).trim_end(), "0123456789");
        assert_eq!(screen.row_text(6).trim_end(), "abcdefghij");
        assert_eq!(screen.row_text(7).trim_end(), "-tail");
        // A wide character that would straddle the edge starts the next row.
        let echo = EchoLine {
            text: "123456789日".to_string(),
            prompt: true,
            ..EchoLine::default()
        };
        let mut screen = Screen::new(size, AmbiguousWidth::Single);
        let (rows, cursor) = lay_echo(&mut screen, 6..8, &echo);
        assert_eq!((rows, cursor), (2, Some(Pos { row: 7, col: 2 })));
        assert_eq!(screen.row_text(7).trim_end(), "日");
        let echo = EchoLine {
            text: "M-x f".to_string(),
            completions: vec!["forward-char".to_string(), "fill".to_string()],
            prompt: true,
        };
        let mut screen = Screen::new(size, AmbiguousWidth::Single);
        assert_eq!(
            lay_echo(&mut screen, 7..8, &echo),
            (1, Some(Pos { row: 7, col: 5 }))
        );
        assert_eq!(screen.row_text(7), "M-x f  {fo");
    }

    /// A tab reaches the next stop of four, as the engine's columns do.
    #[test]
    fn a_tab_reaches_the_next_four_column_stop() {
        for column in 0..32 {
            let cells = usize::from(tab_cells(column));
            assert_eq!(
                cells,
                amacs::editor::editor_grapheme_columns("\t", column),
                "{column}"
            );
        }
    }

    /// The modeline names the buffer, its state, language and caret, and the
    /// program waiting on the edit.
    #[test]
    fn the_modeline_reads_as_emacs_writes_it() {
        let mut modeline = Modeline {
            name: "COMMIT_EDITMSG",
            dirty: true,
            language: Language::GitCommit,
            line: 12,
            column: 4,
            for_word: Some("git"),
        };
        assert_eq!(
            modeline_text(&modeline),
            "-UU-:**- COMMIT_EDITMSG  (GitCommit) L12 C4  [for git · C-c C-c done · C-c C-k abort]"
        );
        modeline.dirty = false;
        modeline.for_word = None;
        assert_eq!(
            modeline_text(&modeline),
            "-UU-:--- COMMIT_EDITMSG  (GitCommit) L12 C4"
        );
    }

    /// A press finds the grapheme drawn under it, the left cell of a wide
    /// one included, and a press past the text finds the row's end.
    #[test]
    fn a_press_finds_the_byte_drawn_under_it() {
        // "a日b" from byte 10: `a` at 0, `日` at 1-2, `b` at 3.
        let row = DrawnRow {
            source: 10..15,
            cells: vec![(0, 0), (1, 1), (4, 3)],
            end: 4,
        };
        assert_eq!(row.byte_at(0), 10);
        assert_eq!(row.byte_at(1), 11);
        assert_eq!(row.byte_at(2), 11);
        assert_eq!(row.byte_at(3), 14);
        assert_eq!(row.byte_at(9), 15);
        assert_eq!(row.column_of(0), 0);
        assert_eq!(row.column_of(4), 3);
        assert_eq!(row.column_of(5), 4);
    }
}
