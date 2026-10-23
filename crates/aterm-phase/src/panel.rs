// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Claude Code Settings panel detection ([`settings_panel`]) and Usage tab row predicates.

use crate::anchors::{anchor, guard_regex};
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

/// The anchors whose row says the tab's figures are not settled: a load or
/// a refresh in flight, or figures the vendor itself calls last-known,
/// partial or unrefreshed.
const UNSETTLED: [&str; 6] = [
    "usage.refreshing",
    "usage.loading",
    "usage.last_known",
    "usage.could_not_refresh",
    "usage.partial",
    "usage.per_model",
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
    USAGE_MARKS.iter().any(|mark| t.starts_with(mark)) || t.starts_with(anchor("usage.loading"))
}

/// Returns `true` if `row` indicates the Usage tab is still loading or unsettled.
#[must_use]
pub fn usage_unsettled(row: &str) -> bool {
    let t = crate::text::trim(row);
    UNSETTLED.iter().any(|id| t.starts_with(anchor(id)))
        || t.starts_with(ERROR_HEAD)
        || t.contains(RATE_LIMITED)
}

/// Returns `true` if `row` is the panel's cancel-hint footer row.
#[must_use]
pub fn panel_tail(row: &str) -> bool {
    crate::text::trim(row).eq_ignore_ascii_case(anchor("prompt.cancel"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(text: &str) -> Vec<String> {
        text.lines().map(str::to_owned).collect()
    }

    /// Rows 1–10 and 21–26 of aterm-agent's `tests/testdata/vendor/2.1.278/screen/usage.txt`,
    /// verbatim.
    const HEAD_2_1_278: &str = "
✻ Sautéed for 6s · done 1:16 PM
▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔
   Settings  Status   Config   Usage   Stats

   Session

   Total cost:            $0.2464
   Total duration (API):  6s
   Total duration (wall): 7s

   Current session · Resets 1:20pm
   (America/Los_Angeles)
   █████████▋

   18% used
";

    /// The measured panel is read under its `▔` rule, three columns in; each negative control
    /// changes one thing (no rule, the header or a row off its column, the composer drawn, a tab
    /// missing or out of order, another title).
    #[test]
    fn the_settings_panel_is_read_under_its_rule_at_its_column() {
        let measured = rows(HEAD_2_1_278);
        assert_eq!(
            settings_panel(&measured),
            Some(SettingsPanel {
                rule: 2,
                header: 3,
                renderer: PanelRenderer::Fullscreen,
            })
        );
        let edited = |edit: &dyn Fn(&mut Vec<String>)| {
            let mut r = measured.clone();
            edit(&mut r);
            settings_panel(&r)
        };
        assert_eq!(edited(&|r| r[2] = String::new()), None, "no rule");
        assert_eq!(
            edited(&|r| r[3] = r[3][1..].to_string()),
            None,
            "one column out"
        );
        assert_eq!(
            edited(&|r| r[3] = format!(" {}", r[3])),
            None,
            "one column in"
        );
        assert_eq!(
            edited(&|r| r.push("$ ls".to_string())),
            None,
            "a row at column zero under it"
        );
        assert_eq!(
            edited(&|r| r[9] = "  Total duration (wall): 7s".to_string()),
            None,
            "a row under it one column out"
        );
        let rule = "─".repeat(56);
        let framed: Vec<String> = [rule.clone(), "❯ ".to_string(), rule.clone()]
            .into_iter()
            .chain(measured.iter().cloned())
            .collect();
        assert!(has_composer_frame(&framed));
        assert_eq!(settings_panel(&framed), None, "the composer is drawn");
        assert!(
            settings_panel(&framed[3..]).is_some(),
            "the same rows without it"
        );
        for (why, header) in [
            ("a tab missing", "   Settings  Status   Usage   Stats"),
            (
                "the tabs out of order",
                "   Settings  Config   Status   Usage   Stats",
            ),
            (
                "the title run on",
                "   Settingsx  Status   Config   Usage   Stats",
            ),
            (
                "another title",
                "   Preferences  Status   Config   Usage   Stats",
            ),
        ] {
            assert_eq!(edited(&|r| r[3] = header.to_string()), None, "{why}");
        }
        for (why, header) in [
            (
                "other spacing",
                "   Settings   Status  Config  Usage  Stats",
            ),
            ("no fourth tab", "   Settings  Status   Config   Usage"),
            (
                "a fifth tab",
                "   Settings  Status   Config   Usage   Stats   More",
            ),
        ] {
            assert!(edited(&|r| r[3] = header.to_string()).is_some(), "{why}");
        }
        assert_eq!(
            edited(&|r| r.insert(3, String::new())).map(|p| (p.rule, p.header)),
            Some((2, 4)),
            "a blank row under the rule"
        );
        let toast = format!("▔▔▔ Update installed {}", "▔".repeat(30));
        assert!(
            edited(&|r| r[2] = toast.clone()).is_some(),
            "a toast in the rule"
        );
    }

    /// Every measured 2.1.287 screen places the panel: fullscreen under the `▔` rule on the
    /// third row, inline under its `─` rule two columns in.
    #[test]
    fn the_panel_is_placed_on_every_measured_screen_of_both_renderers() {
        use crate::prompt::fixtures as fx;
        for (what, text) in [
            ("refreshing", fx::USAGE_REFRESHING),
            ("settled", fx::USAGE_SETTLED),
            ("fold", fx::USAGE_FOLD),
            ("whole", fx::USAGE_WHOLE),
            ("narrow", fx::USAGE_NARROW),
            ("short", fx::USAGE_SHORT),
        ] {
            let screen = fx::screen(text);
            assert_eq!(
                settings_panel(&screen),
                Some(SettingsPanel {
                    rule: 2,
                    header: 3,
                    renderer: PanelRenderer::Fullscreen,
                }),
                "{what}"
            );
            assert_eq!(
                settings_panel(&screen)
                    .expect("placed")
                    .header_guard(&screen),
                "^...Settings..Status...Config...Usage",
                "{what}"
            );
            let reading = crate::read(Some("claude"), &screen, None);
            assert_eq!(
                (
                    reading.phase,
                    reading.phase_authoritative,
                    reading.composer.is_some()
                ),
                (crate::Phase::Idle, false, false),
                "{what}"
            );
        }
        let inline = fx::screen(fx::USAGE_INLINE);
        let panel = settings_panel(&inline).expect("the inline panel");
        assert_eq!(
            panel,
            SettingsPanel {
                rule: 8,
                header: 9,
                renderer: PanelRenderer::Inline,
            }
        );
        assert_eq!(inline[6], "❯ /usage", "left in the transcript above it");
        assert_eq!(
            panel.header_guard(&inline),
            "^..Settings..Status...Config...Usage"
        );
        let scrolled = fx::screen(fx::USAGE_INLINE_SCROLLED);
        assert!(scrolled.iter().any(|row| panel_tail(row)));
        assert_eq!(settings_panel(&scrolled), None);

        let full = fx::screen(fx::USAGE_SETTLED);
        let mut wrong_rule = full.clone();
        wrong_rule[2] = "─".repeat(150);
        assert_eq!(settings_panel(&wrong_rule), None);
        let mut wrong_rule = inline.clone();
        wrong_rule[8] = "▔".repeat(150);
        assert_eq!(settings_panel(&wrong_rule), None);
    }

    /// Every measured screen holds a Usage-tab mark; only the refreshing one holds an unsettled
    /// row, and its last row is the tail too, which is why the settled rule asks for unsettled
    /// rows first.
    #[test]
    fn the_measured_tabs_rows_are_told_by_the_predicates() {
        use crate::prompt::fixtures as fx;
        let last = |rows: &[String]| {
            rows.iter()
                .rev()
                .find(|row| !row.trim().is_empty())
                .cloned()
                .expect("a row")
        };
        for (what, text, unsettled, tail) in [
            ("refreshing", fx::USAGE_REFRESHING, true, true),
            ("settled", fx::USAGE_SETTLED, false, true),
            ("fold", fx::USAGE_FOLD, false, false),
            ("whole", fx::USAGE_WHOLE, false, true),
            ("narrow", fx::USAGE_NARROW, false, true),
            ("short", fx::USAGE_SHORT, false, false),
            ("inline", fx::USAGE_INLINE, false, true),
            ("inline, scrolled", fx::USAGE_INLINE_SCROLLED, false, true),
        ] {
            let screen = fx::screen(text);
            assert!(screen.iter().any(|row| usage_tab_mark(row)), "{what}");
            assert_eq!(
                screen.iter().any(|row| usage_unsettled(row)),
                unsettled,
                "{what}"
            );
            assert_eq!(
                panel_tail(&last(&screen)),
                tail,
                "{what}: {:?}",
                last(&screen)
            );
        }
        let scanning = fx::screen(fx::USAGE_SETTLED);
        let row = scanning
            .iter()
            .find(|row| row.contains("Scanning local sessions"))
            .expect("the block is still scanning on the settled screen");
        assert!(!usage_unsettled(row) && !usage_tab_mark(row));
        assert!(last(&fx::screen(fx::USAGE_FOLD)).ends_with('↓'));
        assert_eq!(last(&fx::screen(fx::USAGE_SHORT)).trim(), "↓");
    }

    /// THE 2.1.301, 2.1.307 AND 2.1.308 TABS MEASURED IN THE LAB
    /// (crates/aterm-lab, 2026-10-15 and 2026-10-21), on both renderers: the tab that drew the
    /// stub's header readings and the enrich error is a panel, a Usage tab
    /// and NOT SETTLED, and its last row (`r to retry · Esc to cancel`) is no
    /// tail; the session block alone (no read made) is settled and ends on
    /// the tail.
    #[test]
    fn the_lab_tabs_read_as_measured() {
        use crate::prompt::fixtures as fx;
        for (what, text, renderer, unsettled, tail) in [
            (
                "header readings, inline",
                fx::USAGE_HEADER_READINGS_INLINE_2_1_301,
                PanelRenderer::Inline,
                true,
                false,
            ),
            (
                "header readings, fullscreen",
                fx::USAGE_HEADER_READINGS_FULLSCREEN_2_1_301,
                PanelRenderer::Fullscreen,
                true,
                false,
            ),
            (
                "session only",
                fx::USAGE_SESSION_ONLY_2_1_301,
                PanelRenderer::Inline,
                false,
                true,
            ),
            (
                "2.1.307 header readings, inline",
                fx::USAGE_HEADER_READINGS_INLINE_2_1_307,
                PanelRenderer::Inline,
                true,
                false,
            ),
            (
                "2.1.307 header readings, fullscreen",
                fx::USAGE_HEADER_READINGS_FULLSCREEN_2_1_307,
                PanelRenderer::Fullscreen,
                true,
                false,
            ),
            (
                "2.1.308 header readings, inline",
                fx::USAGE_HEADER_READINGS_INLINE_2_1_308,
                PanelRenderer::Inline,
                true,
                false,
            ),
            (
                "2.1.308 header readings, fullscreen",
                fx::USAGE_HEADER_READINGS_FULLSCREEN_2_1_308,
                PanelRenderer::Fullscreen,
                true,
                false,
            ),
        ] {
            let screen = fx::screen(text);
            let panel = settings_panel(&screen).unwrap_or_else(|| panic!("{what}: a panel"));
            assert_eq!(panel.renderer, renderer, "{what}");
            let under = &screen[panel.header + 1..];
            assert!(under.iter().any(|row| usage_tab_mark(row)), "{what}");
            assert_eq!(
                under.iter().any(|row| usage_unsettled(row)),
                unsettled,
                "{what}"
            );
            let last = under
                .iter()
                .rev()
                .find(|row| !row.trim().is_empty())
                .expect("a row");
            assert_eq!(panel_tail(last), tail, "{what}: {last:?}");
        }
        for (what, text) in [
            ("menu, inline", fx::USAGE_MENU_INLINE_2_1_301),
            ("menu, fullscreen", fx::USAGE_MENU_FULLSCREEN_2_1_301),
            ("dismissed, inline", fx::USAGE_DISMISSED_INLINE_2_1_301),
            (
                "dismissed, fullscreen",
                fx::USAGE_DISMISSED_FULLSCREEN_2_1_301,
            ),
            ("2.1.307 menu, inline", fx::USAGE_MENU_INLINE_2_1_307),
            (
                "2.1.307 menu, fullscreen",
                fx::USAGE_MENU_FULLSCREEN_2_1_307,
            ),
            (
                "2.1.307 dismissed, inline",
                fx::USAGE_DISMISSED_INLINE_2_1_307,
            ),
            (
                "2.1.307 dismissed, fullscreen",
                fx::USAGE_DISMISSED_FULLSCREEN_2_1_307,
            ),
            ("2.1.308 menu, inline", fx::USAGE_MENU_INLINE_2_1_308),
            (
                "2.1.308 menu, fullscreen",
                fx::USAGE_MENU_FULLSCREEN_2_1_308,
            ),
            (
                "2.1.308 dismissed, inline",
                fx::USAGE_DISMISSED_INLINE_2_1_308,
            ),
            (
                "2.1.308 dismissed, fullscreen",
                fx::USAGE_DISMISSED_FULLSCREEN_2_1_308,
            ),
        ] {
            assert_eq!(settings_panel(&fx::screen(text)), None, "{what}");
        }
    }

    /// The lowest rule is the panel's: a panel quoted above the live one does not move the reading,
    /// and the quoted one alone over a shell line is no panel.
    #[test]
    fn the_lowest_rule_over_a_header_is_the_panels() {
        let measured = rows(HEAD_2_1_278);
        let mut twice = measured.clone();
        twice.extend(measured.iter().skip(2).cloned());
        let panel = settings_panel(&twice).expect("the live panel");
        assert_eq!(measured.len(), 16);
        assert_eq!((panel.rule, panel.header), (16, 17));
        let mut quoted = measured.clone();
        quoted.push("host:work user$ ".to_string());
        assert_eq!(settings_panel(&quoted), None);
    }

    /// The header guard is the observed row, anchored at column zero, one wire token; rows that do
    /// not hold the header give none.
    #[test]
    fn the_header_guard_is_the_observed_row_through_the_usage_tab() {
        let measured = rows(HEAD_2_1_278);
        let panel = settings_panel(&measured).expect("the panel");
        let guard = panel.header_guard(&measured);
        assert_eq!(guard, "^...Settings..Status...Config...Usage");
        assert!(!guard.contains(char::is_whitespace));
        let mut spaced = measured.clone();
        spaced[3] = "   Settings   Status  Config  Usage  Stats".to_string();
        assert_eq!(
            panel.header_guard(&spaced),
            "^...Settings...Status..Config..Usage"
        );
        let mut gone = measured.clone();
        gone[3] = "   Session".to_string();
        assert_eq!(panel.header_guard(&gone), "");
        assert_eq!(panel.header_guard(&measured[..3]), "");
        let inline = SettingsPanel {
            renderer: PanelRenderer::Inline,
            ..panel
        };
        assert_eq!(inline.header_guard(&measured), "", "another column");
    }

    /// The Usage tab's marks, the unsettled rows and the tail, by the vendor's words; beside each,
    /// rows that are not.
    #[test]
    fn the_usage_tabs_rows_are_told_by_the_vendors_words() {
        for row in [
            "   Total cost:            $0.2464",
            "   Current session",
            "   Current session · Resets 1:20pm",
            "   Loading usage data…",
        ] {
            assert!(usage_tab_mark(row), "{row}");
        }
        for row in [
            "   Session",
            "   Current week (all models)",
            "   Version: 2.1.287",
            "   Auto-compact: true",
            "",
        ] {
            assert!(!usage_tab_mark(row), "{row}");
        }
        for row in [
            "   Refreshing…",
            "   Loading usage data…",
            "   Showing last-known usage (rate limited — try again in a moment)",
            "   Could not refresh usage data",
            "   Partial usage data (per-model limits unavailable)",
            "   Per-model breakdown unavailable (rate limited — try again in a moment)",
            "   Error: Failed to load usage data",
            "   Error: Usage endpoint is rate limited. Please try again in a moment.",
            "   the endpoint is rate limited",
        ] {
            assert!(usage_unsettled(row), "{row}");
        }
        for row in [
            "   Current session",
            "   ███▌                                               7% used",
            "   Resets 1:20pm (America/Los_Angeles)",
            "   What's contributing to your limits usage?",
            "   Esc to cancel",
            "   Total cost:            $0.0000",
            "",
        ] {
            assert!(!usage_unsettled(row), "{row}");
        }
        for row in ["   Esc to cancel", "Esc to cancel", "   esc to cancel   "] {
            assert!(panel_tail(row), "{row}");
        }
        for row in [
            "   r to retry · Esc to cancel",
            "   Esc to cancel                                      ↓",
            "   of your usage, not a breakdown                     ↓",
            "   Esc to cancel · Tab to amend",
            "",
        ] {
            assert!(!panel_tail(row), "{row}");
        }
    }
}
