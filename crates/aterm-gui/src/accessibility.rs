// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Accessibility snapshot of the visible terminal grid.
//!
//! A PURE conversion from the rendered cells + cursor into the text and metadata
//! an assistive backend (macOS NSAccessibility / VoiceOver) needs: the visible
//! screen as plain text, a role, a label, the grid size, and the cursor's
//! character offset within that text. It holds no GUI state and calls no platform
//! API, so it is unit-tested directly; `main.rs` builds it from live terminal
//! state each frame and, on macOS, hands the text to the content `NSView`'s
//! accessibility attributes (see `apply_to_ns_view`).
//!
//! The text format is byte-identical to the SIGUSR1 `.txt` snapshot — both go
//! through [`push_visible_row`] — so the AI text snapshot and screen reader
//! consume the same visible-cell projection. This is semantic parity with the
//! app-render input; it does not assert compositor or scanout state.

use aterm_core::terminal::RenderCell;

/// Accessibility role for the terminal content view: an editable text area, the
/// closest standard `AX` role for a terminal grid (VoiceOver reads its value and
/// navigates by line/character).
pub const ROLE_TEXT_AREA: &str = "AXTextArea";

/// Human-facing label announced for the terminal view.
pub const LABEL: &str = "aterm terminal";

/// Append one grid row to `text`: each cell's glyph char (control/NUL → space),
/// up to `cols`, with trailing blanks trimmed, terminated by `'\n'`.
///
/// Shared by the accessibility snapshot and the SIGUSR1 `.txt` snapshot so the two
/// representations are always identical.
pub fn push_visible_row(text: &mut String, cells: &[RenderCell], cols: usize) {
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
#[allow(
    dead_code,
    reason = "the snapshot accessors are consumed by the macOS `a11y-appkit` publisher (an off-by-default feature) and the unit tests; retained as the stable provider API in the default build"
)]
pub struct AccessibleSnapshot {
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

#[allow(
    dead_code,
    reason = "accessors are consumed by the macOS `a11y-appkit` publisher (off-by-default) and tests"
)]
impl AccessibleSnapshot {
    /// Build a snapshot from rendered rows of cells.
    ///
    /// `cursor` is `Some((row, col))` only when the cursor is visible; pass `None`
    /// to omit it (e.g. when hidden via DECTCEM).
    #[must_use]
    pub fn from_cells(
        rows_cells: &[Vec<RenderCell>],
        cols: usize,
        cursor: Option<(usize, usize)>,
    ) -> Self {
        let rows = rows_cells.len();
        let mut text = String::with_capacity(rows * (cols + 1));
        for cells in rows_cells {
            push_visible_row(&mut text, cells, cols);
        }
        Self {
            text,
            rows,
            cols,
            cursor,
        }
    }

    /// The accessibility value: the visible screen text (an `AX` value string).
    #[must_use]
    pub fn value(&self) -> &str {
        &self.text
    }

    /// The accessibility role string.
    #[must_use]
    pub fn role(&self) -> &'static str {
        ROLE_TEXT_AREA
    }

    /// The accessibility label.
    #[must_use]
    pub fn label(&self) -> &'static str {
        LABEL
    }

    /// The cursor's character offset within [`value`](Self::value), suitable for
    /// an `AXSelectedTextRange` (a zero-length selection at the caret), or `None`
    /// when the cursor is hidden.
    ///
    /// The offset accounts for per-row trailing-blank trimming: it sums the
    /// trimmed length of each preceding line (plus its `'\n'`) and clamps the
    /// column to the cursor line's trimmed length (so a caret parked in trailing
    /// whitespace maps to the end of that line's visible text).
    #[must_use]
    pub fn cursor_offset(&self) -> Option<usize> {
        let (crow, ccol) = self.cursor?;
        let mut off = 0usize;
        for (i, line) in self.text.split('\n').enumerate() {
            let line_len = line.chars().count();
            if i == crow {
                return Some(off + ccol.min(line_len));
            }
            off += line_len + 1; // +1 for the '\n'
        }
        None
    }
}

#[cfg(all(target_os = "macos", feature = "a11y-appkit"))]
pub use macos::apply_to_ns_view;

#[cfg(all(target_os = "macos", feature = "a11y-appkit"))]
mod macos {
    use super::AccessibleSnapshot;
    use objc2_app_kit::{NSAccessibility, NSView};
    use objc2_foundation::{NSRange, NSString};
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use winit::window::Window;

    /// Publish `snap` to the window's content `NSView` accessibility attributes so
    /// VoiceOver can read the terminal: role (text area), label, value (the visible
    /// screen text), and the caret as a zero-length `AXSelectedTextRange` (so
    /// VoiceOver tracks the cursor), unless the cursor is hidden.
    ///
    /// Best-effort and side-effect-only: it silently returns if the AppKit handle
    /// is unavailable. Must be called on the main thread (AppKit requirement),
    /// which the winit event loop guarantees. VoiceOver behavior itself is not
    /// machine-verifiable here and is validated manually.
    pub fn apply_to_ns_view(window: &Window, snap: &AccessibleSnapshot) {
        let Ok(handle) = window.window_handle() else {
            return;
        };
        let RawWindowHandle::AppKit(h) = handle.as_raw() else {
            return;
        };
        // SAFETY: `ns_view` points at this window's live NSView (owned by winit for
        // the window's lifetime); we only borrow it on the main thread to set its
        // accessibility attributes — the same borrow pattern as
        // `match_window_colorspace_to_content`.
        let view: &NSView = unsafe { &*(h.ns_view.as_ptr() as *const NSView) };
        let role = NSString::from_str(snap.role());
        let label = NSString::from_str(snap.label());
        let value = NSString::from_str(snap.value());
        // accessibilityValue is typed `id` (Option<&AnyObject>); deref-coerce the
        // NSString (Retained<NSString> → … → AnyObject) at this type-annotated let,
        // since the coercion cannot reach inside the `Some(..)` at the call.
        let value_obj: &objc2::runtime::AnyObject = &value;
        // SAFETY: standard AppKit NSAccessibility setters on a live NSView, on the
        // main thread. `&*role`/`&*label` produce `&NSString` (NSAccessibilityRole
        // is an NSString typedef); the coercion to the parameter type happens at the
        // argument position.
        unsafe {
            view.setAccessibilityRole(Some(&*role));
            view.setAccessibilityLabel(Some(&*label));
            view.setAccessibilityValue(Some(value_obj));
            view.setAccessibilityElement(true);
        }
        // Publish the caret only when the cursor is visible; a hidden cursor
        // (DECTCEM) yields None, and we must not assert a bogus selection.
        if let Some(off) = snap.cursor_offset() {
            // SAFETY: same live main-thread NSView as above; NSRange is a plain
            // #[repr(C)] {location,length} value passed by copy. A zero-length
            // range at `off` is the caret (an empty selection at the cursor).
            unsafe {
                view.setAccessibilitySelectedTextRange(NSRange::new(off, 0));
            }
        }
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
    fn role_and_label_are_stable() {
        let s = snap("", None);
        assert_eq!(s.role(), "AXTextArea");
        assert_eq!(s.label(), "aterm terminal");
    }

    #[test]
    fn value_is_the_text() {
        let s = snap("hello\nworld\n", None);
        assert_eq!(s.value(), "hello\nworld\n");
    }

    #[test]
    fn cursor_offset_none_when_hidden() {
        assert_eq!(snap("abc\n", None).cursor_offset(), None);
    }

    #[test]
    fn cursor_offset_first_line() {
        // caret at row 0 col 2 in "hello" → offset 2.
        assert_eq!(
            snap("hello\nworld\n", Some((0, 2))).cursor_offset(),
            Some(2)
        );
    }

    #[test]
    fn cursor_offset_second_line_accounts_for_newline() {
        // "hello\n" is 6 chars; caret at row 1 col 3 → 6 + 3 = 9.
        assert_eq!(
            snap("hello\nworld\n", Some((1, 3))).cursor_offset(),
            Some(9)
        );
    }

    #[test]
    fn cursor_offset_clamps_into_trailing_whitespace() {
        // Row 0 trimmed to "hi" (len 2); a caret at col 10 clamps to 2.
        assert_eq!(snap("hi\n", Some((0, 10))).cursor_offset(), Some(2));
    }

    #[test]
    fn from_cells_builds_text_from_real_render_cells() {
        // Drive a real engine so we exercise the actual RenderCell shape.
        let mut term = Terminal::new(3, 10);
        term.process(b"hello");
        let cells: Vec<Vec<RenderCell>> = (0..3).map(|r| term.render_row(r)).collect();
        let s = AccessibleSnapshot::from_cells(&cells, 10, Some((0, 5)));
        // First line is "hello" (trailing blanks trimmed); later rows are blank.
        assert!(s.text.starts_with("hello\n"), "got {:?}", s.text);
        assert_eq!(s.rows, 3);
        // Cursor after "hello" → offset 5.
        assert_eq!(s.cursor_offset(), Some(5));
    }

    #[test]
    fn push_visible_row_trims_and_terminates() {
        let mut term = Terminal::new(1, 8);
        term.process(b"ab");
        let row = term.render_row(0);
        let mut text = String::new();
        push_visible_row(&mut text, &row, 8);
        assert_eq!(text, "ab\n"); // trailing 6 blanks trimmed, '\n' added
    }
}
