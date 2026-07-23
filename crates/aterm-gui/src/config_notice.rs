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

use crate::hud_bar;
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

/// PURE grid-cell row builder (the HUD/notice band style): exactly `panel_rows` rows, each
/// exactly `cols` wide so the splice overwrites frame rows in place. Row 0 is a bold
/// title on a seam (the panel's top edge); each following row is one warning, truncated
/// to width, in the theme's WARN/VALUE colours. Reuses `settings::{blank_row,write_str}`
/// so a row is always full-width (the splice debug-asserts `row.len() == cols`).
pub(crate) fn notice_rows(
    lines: &[String],
    cols: usize,
    panel_rows: usize,
    theme: Theme,
) -> Vec<Vec<RenderCell>> {
    let c = hud_bar::hud_colors(theme);
    let mut rows: Vec<Vec<RenderCell>> = (0..panel_rows)
        .map(|r| blank_row(cols, c.label, c.bar_bg, r == 0))
        .collect();
    if panel_rows == 0 {
        return rows;
    }
    let title = format!("config: {} notice(s) (auto-dismiss)", lines.len());
    write_str(&mut rows[0], cols, 1, &title, c.warn, c.bar_bg, true);
    for (i, line) in lines.iter().take(panel_rows.saturating_sub(1)).enumerate() {
        write_str(&mut rows[1 + i], cols, 1, line, c.value, c.bar_bg, false);
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

    #[test]
    fn notice_rows_shape() {
        let lines = vec![
            "first warning".to_string(),
            "x".repeat(200), // longer than cols -> must truncate, no panic
            "third".to_string(),
        ];
        let cols = 80;
        let rows = notice_rows(&lines, cols, 4, Theme::default());
        assert_eq!(rows.len(), 4);
        for row in &rows {
            assert_eq!(row.len(), cols, "every row must be exactly cols wide");
        }
        let row0: String = rows[0].iter().map(|cell| cell.ch).collect();
        assert!(row0.contains("3 notice(s)"), "{row0}");
        // wanted_rows caps at MAX_NOTICE_ROWS even with many lines.
        let many: Vec<String> = (0..20).map(|i| format!("w{i}")).collect();
        assert_eq!(
            ConfigNotice::new(many, Instant::now())
                .unwrap()
                .wanted_rows(),
            MAX_NOTICE_ROWS
        );
    }
}
