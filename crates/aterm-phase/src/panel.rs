// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Claude Code Settings panel detection ([`settings_panel`]) and Usage tab row predicates.

use crate::anchors::guard_regex;
use crate::phase::{has_composer_frame, is_rule};
use crate::prompt::is_panel_rule;

/// The header row's words in drawing order: the title, then the tabs the reader asks for.
const TITLE: &str = "Settings";
const TABS: [&str; 3] = ["Status", "Config", "Usage"];

/// First words of rows only the Usage tab draws: the `Session` block's first figure and the
/// first window's title (measured on 2.1.278 and 2.1.287).
const USAGE_MARKS: [&str; 2] = ["Total cost:", "Current session"];

/// What a failed load draws before its reason, and the words every rate-limited reading
/// carries (read in 2.1.287).
const ERROR_HEAD: &str = "Error:";
const RATE_LIMITED: &str = "rate limited";

/// The anchor texts whose row says the tab's figures are not settled: a load or
/// a refresh in flight, or figures the vendor itself calls last-known,
/// partial or unrefreshed.
const UNSETTLED: [&str; 6] = [
    crate::anchor!("usage.refreshing"),
    crate::anchor!("usage.loading"),
    crate::anchor!("usage.last_known"),
    crate::anchor!("usage.could_not_refresh"),
    crate::anchor!("usage.partial"),
    crate::anchor!("usage.per_model"),
];

/// Claude Code renderer that drew the Settings panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelRenderer {
    /// Alternate-screen fullscreen renderer (`▔` rule, three-column indent).
    Fullscreen,
    /// Main-screen inline renderer (`─` rule, two-column indent).
    Inline,
}

impl PanelRenderer {
    /// Returns the left-margin column indent for this renderer.
    #[must_use]
    pub const fn column(self) -> usize {
        match self {
            PanelRenderer::Fullscreen => 3,
            PanelRenderer::Inline => 2,
        }
    }
}

/// Located Settings panel rule, header row index, and renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingsPanel {
    /// Row index of the full-width rule above the panel.
    pub rule: usize,
    /// Row index of the `Settings  Status   Config   Usage` header row.
    pub header: usize,
    pub renderer: PanelRenderer,
}

impl SettingsPanel {
    /// Builds the `key if=` / `await gone` regex guard for the observed header row.
    #[must_use]
    pub fn header_guard(&self, rows: &[String]) -> String {
        let Some(row) = rows.get(self.header) else {
            return String::new();
        };
        if header_at(row, self.renderer.column()).is_none() {
            return String::new();
        }
        let last = TABS[TABS.len() - 1];
        let Some(at) = row.find(last) else {
            return String::new();
        };
        format!("^{}", guard_regex(&row[..at + last.len()]))
    }
}

/// `Some(())` where `row` is the header drawn `column` columns in: exactly
/// `column` spaces, then the title as a whole word, then the three tabs
/// named in order among the words after it.
fn header_at(row: &str, column: usize) -> Option<()> {
    let indent = row.len() - row.trim_start_matches(' ').len();
    if indent != column {
        return None;
    }
    let mut words = row[indent..].split_whitespace();
    if words.next() != Some(TITLE) {
        return None;
    }
    let mut tabs = TABS.iter();
    let mut want = tabs.next();
    for word in words {
        if Some(&word) == want {
            want = tabs.next();
        }
    }
    want.is_none().then_some(())
}

/// How many columns in a non-blank row starts (its leading spaces).
fn column_of(row: &str) -> usize {
    row.len() - row.trim_start_matches(' ').len()
}

/// Detects a settled Claude Code Settings panel on `rows`.
#[must_use]
pub fn settings_panel(rows: &[String]) -> Option<SettingsPanel> {
    if has_composer_frame(rows) {
        return None;
    }
    (0..rows.len()).rev().find_map(|rule| {
        let renderer = if is_panel_rule(&rows[rule]) {
            PanelRenderer::Fullscreen
        } else if is_rule(&rows[rule]) {
            PanelRenderer::Inline
        } else {
            return None;
        };
        let header = (rule + 1..rows.len()).find(|&i| !crate::text::trim(&rows[i]).is_empty())?;
        header_at(&rows[header], renderer.column())?;
        rows[header + 1..]
            .iter()
            .filter(|row| !crate::text::trim(row).is_empty())
            .all(|row| column_of(row) >= renderer.column())
            .then_some(SettingsPanel {
                rule,
                header,
                renderer,
            })
    })
}

/// Returns `true` if `row` starts with a Usage-tab marker or `Error:`.
#[must_use]
pub fn usage_tab_mark(row: &str) -> bool {
    let t = crate::text::trim(row);
    USAGE_MARKS.iter().any(|mark| t.starts_with(mark))
        || t.starts_with(crate::anchor!("usage.loading"))
}

/// Returns `true` if `row` indicates the Usage tab is still loading or unsettled.
#[must_use]
pub fn usage_unsettled(row: &str) -> bool {
    let t = crate::text::trim(row);
    UNSETTLED.iter().any(|head| t.starts_with(head))
        || t.starts_with(ERROR_HEAD)
        || t.contains(RATE_LIMITED)
}

/// Returns `true` if `row` is the panel's cancel-hint footer row.
#[must_use]
pub fn panel_tail(row: &str) -> bool {
    crate::text::trim(row).eq_ignore_ascii_case(crate::anchor!("prompt.cancel"))
}

#[cfg(test)]
#[path = "panel_tests.rs"]
mod tests;
