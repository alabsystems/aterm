// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Accessibility snapshot of the visible terminal grid.
//!
//! A pure conversion from the rendered cells and cursor into what an assistive
//! backend needs: the visible screen as plain text, a role, a label, the grid size
//! and the cursor's character offset. It holds no GUI state and calls no platform
//! API; the App builds it each frame and, on macOS with `a11y-appkit`, hands it to
//! the content `NSView` (`apply_to_ns_view`). The text goes through
//! [`push_visible_row`], as the SIGUSR1 `.txt` snapshot does, so a screen reader and
//! the text snapshot read the same visible-cell projection.

use aterm_core::terminal::RenderCell;

/// Accessibility role for the terminal content view: an editable text area, the
/// closest standard `AX` role for a terminal grid (VoiceOver reads its value and
/// navigates by line/character).
#[cfg(all(target_os = "macos", feature = "a11y-appkit"))]
pub(crate) const ROLE_TEXT_AREA: &str = "AXTextArea";

/// Human-facing label announced for the terminal view.
#[cfg(any(a11y_tree, all(target_os = "macos", feature = "a11y-appkit")))]
pub(crate) const LABEL: &str = "aterm terminal";

/// Append one grid row to `text`: each cell's glyph char (control/NUL → space),
/// up to `cols`, trailing blanks trimmed, terminated by `'\n'`. Shared by the
/// accessibility snapshot and the SIGUSR1 `.txt` snapshot, so the two are identical.
///
/// One character per cell, wide glyphs included: the engine pads a double-width
/// glyph's continuation cell with a space (`aterm_core::terminal::render_cells`),
/// so `漢 字` reads as separate words. The pad is deliberate: it keeps the character
/// index equal to the grid column, which the caret offset, the published selection
/// span and braille cursor routing all rely on.
pub(crate) fn push_visible_row(text: &mut String, cells: &[RenderCell], cols: usize) {
    for cell in cells.iter().take(cols) {
        text.push(if cell.ch == '\0' || cell.ch.is_control() {
            ' '
        } else {
            cell.ch
        });
    }
    while text.ends_with(' ') {
        text.pop();
    }
    text.push('\n');
}

/// A plain-text + cursor snapshot of the visible grid for assistive technology.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg(any(a11y_tree, all(target_os = "macos", feature = "a11y-appkit"), test))]
pub(crate) struct AccessibleSnapshot {
    /// The visible screen as text: one line per row (trailing blanks trimmed),
    /// rows separated and terminated by `'\n'`.
    pub text: String,
    /// Grid dimensions.
    pub rows: usize,
    /// Grid width in columns.
    pub cols: usize,
    /// Cursor position as `(row, col)`, 0-based, if the cursor is visible.
    pub cursor: Option<(usize, usize)>,
}

#[cfg(any(a11y_tree, all(target_os = "macos", feature = "a11y-appkit"), test))]
impl AccessibleSnapshot {
    /// Build a snapshot from rendered rows of cells.
    ///
    /// `cursor` is `Some((row, col))` only when the cursor is visible; pass `None`
    /// to omit it (e.g. when hidden via DECTCEM).
    #[must_use]
    pub(crate) fn from_cells(
        rows_cells: &[Vec<RenderCell>],
        cols: usize,
        cursor: Option<(usize, usize)>,
    ) -> Self {
        Self::from_cells_in(rows_cells, 0..rows_cells.len(), 0..cols, cursor)
    }

    /// Build a snapshot of one rectangle of the rendered rows: the projection a
    /// split pane needs. A composed split frame carries every pane side by side
    /// on the same rows, so a whole row is two programs' output spliced together;
    /// read through one pane's rectangle of the same cells, it is that pane's
    /// painted line.
    ///
    /// `cursor` is pane-local, `None` when the pane's cursor is off screen. Cells
    /// outside the supplied rows are absent, not padded: a short row yields a
    /// short line.
    #[must_use]
    pub(crate) fn from_cells_in(
        rows_cells: &[Vec<RenderCell>],
        rows: std::ops::Range<usize>,
        cols: std::ops::Range<usize>,
        cursor: Option<(usize, usize)>,
    ) -> Self {
        let width = cols.len();
        let mut text = String::with_capacity(rows.len() * (width + 1));
        for r in rows.clone() {
            let row: &[RenderCell] = rows_cells.get(r).map_or(&[], Vec::as_slice);
            let start = cols.start.min(row.len());
            let end = cols.end.max(cols.start).min(row.len());
            push_visible_row(&mut text, &row[start..end], width);
        }
        Self {
            text,
            rows: rows.len(),
            cols: width,
            cursor,
        }
    }

    /// The accessibility value: the visible screen text (an `AX` value string).
    #[must_use]
    pub(crate) fn value(&self) -> &str {
        &self.text
    }

    /// The accessibility role string.
    #[must_use]
    #[cfg(all(target_os = "macos", feature = "a11y-appkit"))]
    pub(crate) fn role(&self) -> &'static str {
        ROLE_TEXT_AREA
    }

    /// The accessibility label.
    #[must_use]
    #[cfg(all(target_os = "macos", feature = "a11y-appkit"))]
    pub fn label(&self) -> &'static str {
        LABEL
    }

    /// The cursor's UTF-16 offset within [`value`](Self::value), suitable for
    /// an `AXSelectedTextRange` (a zero-length selection at the caret), or `None`
    /// when the cursor is hidden.
    ///
    /// The offset accounts for per-row trailing-blank trimming: it sums the
    /// trimmed length of each preceding line (plus its `'\n'`) and clamps the
    /// column to the cursor line's trimmed length (so a caret parked in trailing
    /// whitespace maps to the end of that line's visible text). The grid uses
    /// one scalar per cell; NSString's NSRange counts an astral scalar as two
    /// UTF-16 units, so translate after finding the cell's text prefix.
    #[must_use]
    #[cfg(any(all(target_os = "macos", feature = "a11y-appkit"), test))]
    pub(crate) fn cursor_offset(&self) -> Option<usize> {
        let (crow, ccol) = self.cursor?;
        let mut off = 0usize;
        for (i, line) in self.text.split('\n').enumerate() {
            if i == crow {
                return Some(off + line.chars().take(ccol).map(char::len_utf16).sum::<usize>());
            }
            off += line.encode_utf16().count() + 1; // +1 for the '\n'
        }
        None
    }
}

#[cfg(all(target_os = "macos", feature = "a11y-appkit"))]
pub(crate) use macos::apply_to_ns_view;

#[cfg(all(target_os = "macos", feature = "a11y-appkit"))]
mod macos {
    use aterm_objc::{Id, NSRange, Sel, autoreleasepool, sel};
    use winit::window::Window;

    use super::AccessibleSnapshot;
    use crate::appkit;

    /// Publish `snap` to the window's content `NSView` accessibility attributes so
    /// VoiceOver can read the terminal: role, label, value (the visible text) and,
    /// unless the cursor is hidden, the caret as a zero-length `AXSelectedTextRange`.
    ///
    /// Best-effort: returns silently without an AppKit handle. Main thread only
    /// (the winit event loop's). VoiceOver behaviour is validated by hand.
    pub(crate) fn apply_to_ns_view(window: &Window, snap: &AccessibleSnapshot) {
        let view = crate::platform::ns_view_of(window);
        if view.is_null() {
            return;
        }
        autoreleasepool(|_| {
            let (Some(role), Some(label), Some(value)) = (
                appkit::nsstring(snap.role()),
                appkit::nsstring(snap.label()),
                appkit::nsstring(snap.value()),
            ) else {
                return;
            };
            // SAFETY: `view` is this window's live NSView (winit owns it for the
            // window's lifetime), used on the main thread. `-setAccessibilityRole:` /
            // `-setAccessibilityLabel:` / `-setAccessibilityValue:` take one object
            // (the role typedef is an NSString) and `-setAccessibilityElement:` a BOOL.
            unsafe {
                appkit::send_v_id(view, sel!(setAccessibilityRole:), role.id());
                appkit::send_v_id(view, sel!(setAccessibilityLabel:), label.id());
                appkit::send_v_id(view, sel!(setAccessibilityValue:), value.id());
                appkit::send_v_bool(view, sel!(setAccessibilityElement:), true);
            }
            // Publish the caret only when the cursor is visible; a hidden cursor
            // (DECTCEM) yields None, and we must not assert a bogus selection.
            if let Some(off) = snap.cursor_offset() {
                // SAFETY: same live main-thread NSView; `-setAccessibility
                // SelectedTextRange:` is `-(void)(NSRange)` and `NSRange` is a
                // plain `#[repr(C)]` `{location,length}` pair passed by copy —
                // sixteen bytes, so it goes in registers on both ABIs. A
                // zero-length range at `off` is the caret.
                unsafe {
                    let set: unsafe extern "C-unwind" fn(Id, Sel, NSRange) =
                        aterm_objc::checked_msg();
                    set(
                        view,
                        sel!(setAccessibilitySelectedTextRange:),
                        NSRange {
                            location: off,
                            length: 0,
                        },
                    );
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_core::terminal::Terminal;

    fn snap(text: &str, cursor: Option<(usize, usize)>) -> AccessibleSnapshot {
        AccessibleSnapshot {
            text: text.to_string(),
            rows: 0,
            cols: 0,
            cursor,
        }
    }

    #[test]
    fn cursor_offset_counts_utf16_units_to_the_caret() {
        for (label, text, cursor, expected) in [
            ("hidden caret", "abc\n", None, None),
            // caret at row 0 col 2 in "hello" → offset 2.
            ("first line", "hello\nworld\n", Some((0, 2)), Some(2)),
            ("astral glyph before caret", "😀 x\n", Some((0, 2)), Some(3)),
            (
                "astral glyph on earlier row",
                "😀\nx\n",
                Some((1, 1)),
                Some(4),
            ),
            ("astral trailing clamp", "😀\n", Some((0, 10)), Some(2)),
            // "hello\n" is 6 chars; caret at row 1 col 3 → 6 + 3 = 9.
            (
                "second line counts the newline",
                "hello\nworld\n",
                Some((1, 3)),
                Some(9),
            ),
            // Row 0 trimmed to "hi" (len 2); a caret at col 10 clamps to 2.
            (
                "clamps into trailing whitespace",
                "hi\n",
                Some((0, 10)),
                Some(2),
            ),
        ] {
            assert_eq!(snap(text, cursor).cursor_offset(), expected, "{label}");
        }
    }

    /// A pane's rectangle reads only its own columns of the composed split frame.
    #[test]
    fn a_pane_rectangle_reads_only_its_own_columns() {
        let mut term = Terminal::new(2, 21);
        term.process(b"left\x1b[11G\xe2\x96\x90right");
        let cells: Vec<Vec<RenderCell>> = (0..2).map(|r| term.render_row(r)).collect();

        let whole = AccessibleSnapshot::from_cells(&cells, 21, None);
        let composed = whole.value().lines().next().unwrap();
        assert!(
            composed.contains("left") && composed.contains("right"),
            "the composed row carries both panes: {composed:?}"
        );

        let left = AccessibleSnapshot::from_cells_in(&cells, 0..2, 0..10, None);
        let right = AccessibleSnapshot::from_cells_in(&cells, 0..2, 11..21, Some((0, 5)));
        assert_eq!(left.value(), "left\n\n", "no divider, no neighbour");
        assert_eq!(right.value(), "right\n\n");
        assert_eq!((left.rows, left.cols), (2, 10));
        assert_eq!(right.cursor_offset(), Some(5), "the pane-local caret");
    }
}
