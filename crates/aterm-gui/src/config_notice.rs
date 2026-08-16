// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A transient, auto-dismissing in-window banner that surfaces config-load/reload
//! NOTICES — chiefly dropped `[key_sequences]`/`[keybindings]` rules (bad chord/escape/
//! empty/oversized value/unknown action), plus restart-only edits that a live reload
//! can't apply (`columns`/`lines`; see `app_config::restart_notices`). stderr is
//! invisible to a Finder-launched .app, so a fat-fingered or restart-only config edit
//! would otherwise take no effect with zero feedback.
//!
//! Pure + clock-injected (mirrors `aterm-predict`): the lifecycle (TTL/expiry) and the row
//! builder take an explicit `now`/inputs so they unit-test without a window. The banner
//! is drawn by `App::splice_config_notice` (app_render.rs), which OVERWRITES the top
//! rows in place — no geometry change — exactly like the settings panel.

use std::time::{Duration, Instant};

use aterm_core::terminal::RenderCell;
use aterm_render::Theme;

use crate::chrome_band;
use crate::settings::{blank_row, write_str};

/// How long a config-warning banner stays up before auto-dismissing.
pub(crate) const NOTICE_TTL: Duration = Duration::from_secs(8);

/// Cap the banner height so a config with many bad rules can't paper over the screen.
const MAX_NOTICE_ROWS: usize = 6;

/// A transient in-window banner listing config-load/reload warnings. GLOBAL (config is
/// window-uniform); painted into every window's frame; auto-expires after [`NOTICE_TTL`].
pub(crate) struct ConfigNotice {
    pub(crate) lines: Vec<String>,
    until: Instant,
}

impl ConfigNotice {
    /// Build from collected warnings; `None` when there are none (so no banner).
    pub(crate) fn new(lines: Vec<String>, now: Instant) -> Option<Self> {
        if lines.is_empty() {
            None
        } else {
            Some(Self {
                lines,
                until: now + NOTICE_TTL,
            })
        }
    }

    /// The instant the banner auto-dismisses (folded into the event-loop wait deadline).
    pub(crate) fn deadline(&self) -> Instant {
        self.until
    }

    pub(crate) fn is_expired(&self, now: Instant) -> bool {
        now >= self.until
    }

    /// Rows to paint: a title row + one row per warning, capped at [`MAX_NOTICE_ROWS`].
    pub(crate) fn wanted_rows(&self) -> usize {
        (self.lines.len() + 1).min(MAX_NOTICE_ROWS)
    }
}

/// The banner's left margin and the extra indent its warning list hangs at, in cells.
const MARGIN: usize = 2;
const BULLET_INDENT: usize = MARGIN + 2;

/// `s` cut to `max` cells with a trailing ellipsis when it does not fit. A hard cut ends
/// a warning mid-token and reads like corruption; an ellipsis says "there is more".
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

/// PURE grid-cell row builder: exactly `panel_rows` rows, each
/// exactly `cols` wide so the splice overwrites frame rows in place. Row 0 is a bold
/// title on a seam (the panel's top edge) with a right-aligned "it goes away by itself"
/// hint; each following row is one warning, bulleted, hanging-indented and ellipsized to
/// width, in the theme's WARN/VALUE colours. When the list does not fit, the last row
/// COUNTS what was dropped rather than silently ending — a banner that shows 5 of 20
/// problems while looking complete is worse than no banner. Reuses
/// `settings::{blank_row,write_str}` so a row is always full-width (the splice
/// debug-asserts `row.len() == cols`).
pub(crate) fn notice_rows(
    lines: &[String],
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
    // "config: 3 notice(s) (auto-dismiss)" said the same thing three ways and made the
    // user parse a plural stub. The mark carries the severity, the count carries the
    // scale, and the fact that it leaves on its own is a quiet aside on the right.
    // The mark stays inside `write_str`'s stated contract — ASCII and a few narrow BMP
    // glyphs, one char per CELL. A warning triangle would be ambiguous-width and could
    // bleed into the next cell on the terminals that treat it as emoji, which is a poor
    // trade for a mark whose severity the warn colour already carries.
    let title = format!(
        "!  Config \u{00B7} {} {}",
        lines.len(),
        if lines.len() == 1 {
            "notice"
        } else {
            "notices"
        }
    );
    write_str(&mut rows[0], cols, MARGIN, &title, c.warn, c.bar_bg, true);
    const HINT: &str = "dismisses on its own";
    let hint_col = cols.saturating_sub(HINT.len() + MARGIN);
    if hint_col > MARGIN + title.chars().count() + 2 {
        write_str(&mut rows[0], cols, hint_col, HINT, c.label, c.bar_bg, false);
    }

    // Capacity for warning rows, reserving one for the "+N more" tally when the list
    // overflows and there is room to say so.
    let capacity = panel_rows.saturating_sub(1);
    let overflow = lines.len() > capacity;
    let shown = if overflow && capacity >= 2 {
        capacity - 1
    } else {
        capacity
    };
    let text_w = cols.saturating_sub(BULLET_INDENT + MARGIN);
    for (i, line) in lines.iter().take(shown).enumerate() {
        write_str(
            &mut rows[1 + i],
            cols,
            MARGIN,
            "\u{2022}",
            c.label,
            c.bar_bg,
            false,
        );
        write_str(
            &mut rows[1 + i],
            cols,
            BULLET_INDENT,
            &ellipsized(line, text_w),
            c.value,
            c.bar_bg,
            false,
        );
    }
    if overflow && shown < capacity {
        let more = format!("+{} more \u{2014} see the log", lines.len() - shown);
        write_str(
            &mut rows[1 + shown],
            cols,
            BULLET_INDENT,
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

    #[test]
    fn new_none_when_empty() {
        let now = Instant::now();
        assert!(ConfigNotice::new(vec![], now).is_none());
        assert!(ConfigNotice::new(vec!["x".into()], now).is_some());
    }

    #[test]
    fn expiry_after_ttl() {
        let t0 = Instant::now();
        let n = ConfigNotice::new(vec!["x".into()], t0).unwrap();
        assert!(!n.is_expired(t0));
        assert!(!n.is_expired(t0 + NOTICE_TTL - Duration::from_millis(1)));
        assert!(n.is_expired(t0 + NOTICE_TTL));
        assert_eq!(n.deadline(), t0 + NOTICE_TTL);
    }

    fn text_of(row: &[RenderCell]) -> String {
        row.iter().map(|cell| cell.ch).collect()
    }

    #[test]
    fn notice_rows_shape() {
        let lines = vec![
            "first warning".to_string(),
            "x".repeat(200), // longer than cols -> must ellipsize, no panic
            "third".to_string(),
        ];
        let cols = 80;
        let rows = notice_rows(&lines, cols, 4, Theme::default());
        assert_eq!(rows.len(), 4);
        for row in &rows {
            assert_eq!(row.len(), cols, "every row must be exactly cols wide");
        }
        let row0 = text_of(&rows[0]);
        assert!(row0.contains("3 notices"), "{row0}");
        assert!(row0.contains("dismisses on its own"), "{row0}");
        // Each warning is bulleted and hangs at one indent.
        let row1 = text_of(&rows[1]);
        assert!(row1.trim_start().starts_with('\u{2022}'), "{row1:?}");
        assert!(row1.contains("first warning"), "{row1:?}");
        // The over-long warning is cut with an ellipsis, not mid-token.
        let row2 = text_of(&rows[2]);
        assert!(row2.contains('\u{2026}'), "{row2:?}");
        // wanted_rows caps at MAX_NOTICE_ROWS even with many lines.
        let many: Vec<String> = (0..20).map(|i| format!("w{i}")).collect();
        assert_eq!(
            ConfigNotice::new(many, Instant::now())
                .unwrap()
                .wanted_rows(),
            MAX_NOTICE_ROWS
        );
    }

    /// A CAPPED LIST MUST SAY IT IS CAPPED. Five of twenty warnings shown with no tally
    /// reads as "these are all of them", and the user fixes five of their twenty broken
    /// key bindings.
    #[test]
    fn an_overflowing_list_counts_what_it_dropped() {
        let lines: Vec<String> = (0..20).map(|i| format!("warning {i}")).collect();
        let rows = notice_rows(&lines, 80, MAX_NOTICE_ROWS, Theme::default());
        let last = text_of(&rows[MAX_NOTICE_ROWS - 1]);
        // 6 rows = title + 4 warnings + the tally for the remaining 16.
        assert!(last.contains("+16 more"), "{last:?}");
        assert!(
            text_of(&rows[4]).contains("warning 3"),
            "four warnings shown"
        );
        // A list that FITS says nothing about overflow.
        let rows = notice_rows(&lines[..3], 80, 4, Theme::default());
        assert!(!text_of(&rows[3]).contains("more"), "no spurious tally");
    }

    /// Degenerate geometry must not panic: one row, zero columns, a single warning wider
    /// than the window.
    #[test]
    fn degenerate_geometry_never_panics() {
        for cols in [0_usize, 1, 3, 8, 200] {
            for panel_rows in [0_usize, 1, 2, 6] {
                let lines = vec!["y".repeat(300), "z".into()];
                let rows = notice_rows(&lines, cols, panel_rows, Theme::default());
                assert_eq!(rows.len(), panel_rows);
                for row in &rows {
                    assert_eq!(row.len(), cols);
                }
            }
        }
    }

    #[test]
    fn ellipsized_only_cuts_what_overflows() {
        assert_eq!(ellipsized("short", 10), "short");
        assert_eq!(ellipsized("exactly-ten", 11), "exactly-ten");
        assert_eq!(ellipsized("abcdef", 3), "ab\u{2026}");
        assert_eq!(ellipsized("abcdef", 0), "");
        // Multi-byte input is counted in CHARS, not bytes.
        assert_eq!(
            ellipsized("\u{00e9}\u{00e9}\u{00e9}", 4),
            "\u{00e9}\u{00e9}\u{00e9}"
        );
    }
}
