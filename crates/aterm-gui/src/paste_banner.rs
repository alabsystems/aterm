// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The multi-line-paste confirmation banner for platforms with no native alert (macOS asks
//! through a window sheet, Windows through a `MessageBoxW`).

use aterm_core::terminal::RenderCell;
#[cfg(test)]
use aterm_render::Theme;

use crate::chrome_band;
use crate::settings::{blank_row, write_str};

/// Banner height without a separate keys row: the title plus up to [`MAX_PREVIEW_LINES`]
/// preview rows, the last of which becomes the "+N more" tally when the paste is longer.
const MAX_BANNER_ROWS: usize = 5;
/// How many lines of the pending paste are previewed at most.
const MAX_PREVIEW_LINES: usize = MAX_BANNER_ROWS - 1;

/// The banner's left margin and the preview's hanging indent, in cells.
const MARGIN: usize = 2;
const PREVIEW_INDENT: usize = MARGIN + 2;

/// One parked multi-line paste awaiting an answer.
pub(crate) struct PendingPaste {
    /// The window the paste targets; only its keys answer, and the entry dies with the window.
    pub(crate) wid: crate::WindowId,
    /// The terminal session that fronted `wid` at the paste gesture, and the only one the answer
    /// may reach.
    pub(crate) session: Option<u64>,
    text: String,
    source: crate::input::Source,
    /// The DEC 2004 (bracketed paste) framing the question was asked under: the program can
    /// flip the mode while the banner stands, and the paste must reach the PTY framed as asked.
    framing: crate::input::PasteFraming,
    /// This question's serial, unique in the process and never `0`: the
    /// repaint key's term for it (`App::question_key`).
    serial: u64,
}

/// The next question serial (shared with `close_confirm`), never `0`.
pub(crate) fn next_question_serial() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

impl PendingPaste {
    #[cfg(any(test, not(any(target_os = "macos", windows))))]
    pub(crate) fn new(
        wid: crate::WindowId,
        session: Option<u64>,
        text: String,
        source: crate::input::Source,
        framing: crate::input::PasteFraming,
    ) -> Self {
        Self {
            wid,
            session,
            text,
            source,
            framing,
            serial: next_question_serial(),
        }
    }

    /// This question's serial (never `0`).
    pub(crate) const fn serial(&self) -> u64 {
        self.serial
    }

    /// Surrender the parked paste for confirmed delivery: the window, the owning session, the
    /// text, its source and the framing the question was asked under.
    pub(crate) fn take(
        self,
    ) -> (
        crate::WindowId,
        Option<u64>,
        String,
        crate::input::Source,
        crate::input::PasteFraming,
    ) {
        (self.wid, self.session, self.text, self.source, self.framing)
    }

    /// The parked text, for the row builder.
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// Rows the banner wants in a window `cols` wide: title + preview (capped at
    /// [`MAX_PREVIEW_LINES`]), plus a row for the answer keys when they do not fit
    /// beside the question, so a narrow window still previews every line it would.
    pub(crate) fn wanted_rows(&self, cols: usize) -> usize {
        let keys_row = usize::from(title_row(&self.text, cols).1.is_none());
        paste_lines(&self.text).len().min(MAX_PREVIEW_LINES) + 1 + keys_row
    }
}

/// `s` cut to `max` cells, ending in an ellipsis when it does not fit.
pub(crate) fn ellipsized(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('\u{2026}');
    out
}

/// One preview line with every control character shown as a middle dot: a hostile clipboard's
/// ESC/C1 bytes and tabs must neither corrupt the band's cells (`write_str` writes one char per
/// cell) nor render as nothing.
fn sanitized(line: &str) -> String {
    line.chars()
        .map(|c| if c.is_control() { '\u{00b7}' } else { c })
        .collect()
}

/// The two keys that answer the question, right-aligned on the title row and spoken as the a11y
/// alert's description: one constant, so the banner and the screen reader offer the same answers.
pub(crate) const ANSWER_KEYS: &str = "Enter pastes \u{00b7} Esc cancels";

/// [`ANSWER_KEYS`] for a window too narrow to carry the verbs beside the question.
const ANSWER_KEYS_SHORT: &str = "Enter \u{00b7} Esc";

/// The most of a line a screen reader is asked to speak: enough to judge a command by.
#[cfg(a11y_tree)]
const SPOKEN_LINE_MAX: usize = 200;

/// Why the paste is asked about: the guard fires only when bracketed paste is off,
/// so each line end submits.
pub(crate) const RISK: &str = "Each line may run as a command.";

/// The accessible alert's description: the risk, the answer keys, then the lines a banner with room
/// for them previews, since a person is asked to approve them. A line stops at [`SPOKEN_LINE_MAX`]
/// rather than a window's width.
#[cfg(a11y_tree)]
pub(crate) fn announcement_detail(text: &str) -> String {
    let lines = paste_lines(text);
    let shown = if lines.len() > MAX_PREVIEW_LINES {
        MAX_PREVIEW_LINES - 1
    } else {
        lines.len()
    };
    let mut detail = format!("{RISK} {ANSWER_KEYS}");
    for line in lines.iter().take(shown) {
        detail.push('\n');
        detail.push_str(&ellipsized(&sanitized(line), SPOKEN_LINE_MAX));
    }
    if lines.len() > shown {
        detail.push_str(&format!("\n+{} more lines", lines.len() - shown));
    }
    detail
}

/// Row 0's title and the answer keys that fit beside it in `cols`: the risk goes
/// first, then the verbs; `None` when even the short keys do not fit, and they take
/// a row of their own.
fn title_row(text: &str, cols: usize) -> (String, Option<&'static str>) {
    let question = format!("!  {}", question(text));
    let with_risk = format!("{question}  {RISK}");
    let fits = |title: &str, keys: &str| {
        cols.saturating_sub(keys.chars().count() + MARGIN) > MARGIN + title.chars().count() + 2
    };
    if fits(&with_risk, ANSWER_KEYS) {
        return (with_risk, Some(ANSWER_KEYS));
    }
    let keys = [ANSWER_KEYS, ANSWER_KEYS_SHORT]
        .into_iter()
        .find(|keys| fits(&question, keys));
    (question, keys)
}

/// The question the banner asks about `text`, as one sentence.
pub(crate) fn question(text: &str) -> String {
    format!("Paste {}?", lines_words(paste_lines(text).len()))
}

/// `1 line` / `N lines`: the count every confirmation names (the banner, the macOS sheet, the
/// Windows dialog).
pub(crate) fn lines_words(n: usize) -> String {
    if n == 1 {
        "1 line".to_string()
    } else {
        format!("{n} lines")
    }
}

/// The lines a paste would enter, split the way a shell reads them: LF, CRLF
/// and a lone CR each end one.
pub(crate) fn paste_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let body = text.strip_suffix('\n').unwrap_or(text);
    let body = body.strip_suffix('\r').unwrap_or(body);
    let mut parts = body.split('\n').peekable();
    let mut lines = Vec::new();
    while let Some(line) = parts.next() {
        let line = if parts.peek().is_some() {
            line.strip_suffix('\r').unwrap_or(line)
        } else {
            line
        };
        lines.extend(line.split('\r'));
    }
    lines
}

/// Pure row builder: exactly `panel_rows` rows, each exactly `cols` wide, so the splice
/// overwrites frame rows in place.
pub(crate) fn banner_rows(
    text: &str,
    cols: usize,
    panel_rows: usize,
    palette: impl Into<chrome_band::ChromePalette>,
) -> Vec<Vec<RenderCell>> {
    let c = chrome_band::band_colors(palette);
    let mut rows: Vec<Vec<RenderCell>> = (0..panel_rows)
        .map(|r| blank_row(cols, c.label, c.bar_bg, r == 0))
        .collect();
    if panel_rows == 0 {
        return rows;
    }
    let lines = paste_lines(text);
    let (title, row0_keys) = title_row(text, cols);
    let mut first_preview = 1;
    if let Some(keys) = row0_keys {
        write_str(&mut rows[0], cols, MARGIN, &title, c.warn, c.bar_bg, true);
        let keys_col = cols.saturating_sub(keys.chars().count() + MARGIN);
        write_str(&mut rows[0], cols, keys_col, keys, c.value, c.bar_bg, true);
    } else if panel_rows > 1 {
        write_str(&mut rows[0], cols, MARGIN, &title, c.warn, c.bar_bg, true);
        let keys = if ANSWER_KEYS.chars().count() + 2 * MARGIN <= cols {
            ANSWER_KEYS
        } else {
            ANSWER_KEYS_SHORT
        };
        write_str(&mut rows[1], cols, MARGIN, keys, c.value, c.bar_bg, true);
        first_preview = 2;
    } else {
        let keys_col = cols.saturating_sub(ANSWER_KEYS_SHORT.chars().count() + MARGIN);
        let title = ellipsized(&title, keys_col.saturating_sub(MARGIN + 1));
        write_str(&mut rows[0], cols, MARGIN, &title, c.warn, c.bar_bg, true);
        write_str(
            &mut rows[0],
            cols,
            keys_col,
            ANSWER_KEYS_SHORT,
            c.value,
            c.bar_bg,
            true,
        );
    }

    let capacity = panel_rows.saturating_sub(first_preview);
    let overflow = lines.len() > capacity;
    let shown = if overflow && capacity >= 2 {
        capacity - 1
    } else {
        capacity
    };
    let text_w = cols.saturating_sub(PREVIEW_INDENT + MARGIN);
    for (i, line) in lines.iter().take(shown).enumerate() {
        write_str(
            &mut rows[first_preview + i],
            cols,
            MARGIN,
            "\u{2502}",
            c.label,
            c.bar_bg,
            false,
        );
        write_str(
            &mut rows[first_preview + i],
            cols,
            PREVIEW_INDENT,
            &ellipsized(&sanitized(line), text_w),
            c.value,
            c.bar_bg,
            false,
        );
    }
    if overflow && shown < capacity {
        let more = format!("+{} more lines", lines.len() - shown);
        write_str(
            &mut rows[first_preview + shown],
            cols,
            PREVIEW_INDENT,
            &ellipsized(&more, text_w),
            c.label,
            c.bar_bg,
            false,
        );
    }
    chrome_band::seal_band_top(&mut rows[0], c.seam);
    rows
}

#[cfg(test)]
#[path = "paste_banner_tests.rs"]
mod tests;
