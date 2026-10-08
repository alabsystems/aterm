// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Claude Code's SETTINGS PANEL — the dialog `/usage`, `/status`, `/config`
//! and `/stats` open, one container with the tabs `Status`, `Config`,
//! `Usage`, `Stats` — as pure functions over a screen's rows: whether the
//! panel is what the screen shows ([`settings_panel`]), and what a row under
//! its header says of the Usage tab ([`usage_tab_mark`],
//! [`usage_unsettled`], [`panel_tail`]).
//!
//! It is here for the accounts probe (`docs/SPEC-claude-accounts-probe-
//! 2026-10-01.md`, P9, P10, P13): the supervisor types `/usage`, reads the
//! panel only once it is SETTLED, and closes it with an Escape fenced by the
//! panel's own header row ([`SettingsPanel::header_guard`]). The settled
//! rule itself is `aterm_agent::harness::usage::usage_panel_read`, composed
//! from the predicates here; the words are the vendor's, so they live in
//! this crate.
//!
//! WHAT IS MEASURED AND WHAT IS READ.
//!
//! * MEASURED on Claude Code 2.1.287, 2026-10-01, by the probe's live check
//!   (the spec's §6: `/usage` typed at an idle composer of a private
//!   headless aterm, the grid read every quarter second; the screens are
//!   `prompt::fixtures::USAGE_*`, at 150x50, 150x80, 110x32, 60x50 and under
//!   the inline renderer):
//!   - THE FULLSCREEN RENDERER draws the panel in the composer's place,
//!     under a full-width rule of `▔` (U+2594) from column zero; the row
//!     right under the rule is `   Settings  Status   Config   Usage
//!     Stats`, THREE columns in; every row under it is three columns in; no
//!     composer frame is on the screen. (The same on 2.1.278, 2026-09-22:
//!     aterm-agent's `tests/testdata/vendor/2.1.278/screen/usage.txt`.)
//!   - THE INLINE RENDERER (`CLAUDE_CODE_NO_FLICKER=0`, the main screen)
//!     draws it under a full-width `─` rule, TWO columns in, with `❯ /usage`
//!     left in the transcript above — and once the panel grows past the
//!     grid the main screen SCROLLS: the rule and the header leave it while
//!     the cancel hint stays (`USAGE_INLINE_SCROLLED`). A panel like that
//!     cannot be placed, and [`settings_panel`] says there is none.
//!   - THE USAGE TAB OPENS UNSETTLED: 255 ms after the Enter it showed the
//!     home's last-known windows with `Refreshing…` above the cancel hint
//!     (the week at 9 %, no Fable window); a quarter second later the
//!     fetched ones (10 %, the Fable window drawn). The figures on an
//!     unsettled panel are the ones being replaced.
//!   - THE TAIL IS ON THE GRID ONLY FOR A MOMENT ON AN ORDINARY PANE. The
//!     tab draws its "contributing" block as `Scanning local sessions…`
//!     first — the whole tab then fits 50 rows, the cancel hint last — and
//!     one to two and a half seconds later (four runs) the scan fills the
//!     block in, which at 50 rows (and at 47) pushes the hint under the
//!     fold: the last row then ends in the vendor's `↓`. At 80 rows the
//!     whole tab stays on the grid; at 32 the tail is under the fold from
//!     the first read. `Scanning local sessions…` is the block's own word
//!     and says nothing of the windows.
//!   - The reader of this crate reads a panel screen `Idle`, NOT
//!     authoritative, with no composer (`reader::read` over each capture).
//!   - On Escape the inline renderer leaves `⎿  Settings dialog dismissed`
//!     under the `❯ /usage` row; the fullscreen renderer leaves no row.
//! * READ in the 2.1.287 bundle (the plan-limits component; the spec's §0),
//!   NOT seen on a screen: with no figures at all the tab draws `Loading
//!   usage data…` over the cancel hint; a failed load draws `Error: <why>`
//!   (why: `Failed to load usage data`, or `Usage endpoint is rate limited.
//!   Please try again in a moment.`) over a retry hint and the cancel hint;
//!   an enrich error draws one line under the usage-credits block (`Showing
//!   last-known usage (…)`, `Could not refresh usage data`, `Partial usage
//!   data (…)`, `Per-model breakdown unavailable (…)`) and puts the retry
//!   hint before the cancel hint. The draw order is the bundle's and the
//!   captures agree with it as far as they go: the windows, a credit block,
//!   the "contributing" block, usage credits, the enrich-error line,
//!   `Refreshing…`, and LAST the hint row. The `…` is an escape in the
//!   bundle, so the anchors carry the words without it.
//! * NOT MEASURED: the loading and error states, any enrich error, the
//!   other three tabs (never opened: the Status tab names the login), two
//!   accounts. A fixture of one of those states is COMPOSED and says so.

use crate::anchors::{anchor, guard_regex};
use crate::phase::{has_composer_frame, is_rule};
use crate::prompt::is_panel_rule;

/// The header row's own words, in the order the vendor draws them: the
/// container's title, then the tabs the reader asks for. `Stats` is the
/// fourth tab (MEASURED) and is not asked for: the three before it place the
/// row, and a tab added after them changes nothing here.
///
/// NOT ROWS OF [`crate::anchors::ANCHORS`], on purpose. `grep_guard`'s V1
/// fences a ONE-WORD anchor as a quoted literal anywhere in shipped Rust, and
/// aterm's own Settings surface spells its title so in half a dozen files:
/// the word as an anchor would fail the guard on aterm's own UI. Nothing
/// outside this module needs the words — a caller that guards on the header
/// row asks [`SettingsPanel::header_guard`].
const TITLE: &str = "Settings";
const TABS: [&str; 3] = ["Status", "Config", "Usage"];

/// The Usage tab's own rows, by their first words (MEASURED on 2.1.278 and
/// 2.1.287: the `Session` block's first figure and the first window's title;
/// the third mark is the loading line, [`anchor`] `usage.loading`, READ in
/// 2.1.287).
/// One of them under the header is what says the tab drawn is Usage and not
/// Status, Config or Stats.
const USAGE_MARKS: [&str; 2] = ["Total cost:", "Current session"];

/// What a failed load draws before its reason (READ in 2.1.287:
/// `children:["Error: ",L]`), and the words every rate-limited reading
/// carries (`Usage endpoint is rate limited. …`, `… (rate limited — try
/// again in a moment)`).
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

/// Which of Claude Code's two renderers drew the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelRenderer {
    /// The fullscreen renderer (the alternate screen): the panel under a
    /// `▔` rule, three columns in. MEASURED.
    Fullscreen,
    /// The inline renderer (the main screen, `CLAUDE_CODE_NO_FLICKER=0`):
    /// the panel under a `─` rule, two columns in. MEASURED.
    Inline,
}

impl PanelRenderer {
    /// How many columns in the renderer draws the panel's rows.
    #[must_use]
    pub fn column(self) -> usize {
        match self {
            PanelRenderer::Fullscreen => 3,
            PanelRenderer::Inline => 2,
        }
    }
}

/// The Settings panel on a screen: where its rule and its header row are,
/// and which renderer drew it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingsPanel {
    /// The index of the full-width rule over the panel.
    pub rule: usize,
    /// The index of the header row (`Settings  Status   Config   Usage …`).
    pub header: usize,
    pub renderer: PanelRenderer,
}

impl SettingsPanel {
    /// The `key if=` / `await gone` guard for the header row AS OBSERVED on
    /// `rows`: `^`, the row from its first column through the `Usage` tab's
    /// name, every space spelled `.` ([`guard_regex`]) — so the guard is one
    /// wire token, anchored at column zero, and matches the row whichever
    /// spacing the vendor drew between the tabs. A copy of the row quoted
    /// further in (a tool's output under `⎿`, the worker's own words) is at
    /// another column and does not match. Empty where `rows` does not hold
    /// the header this value names: the caller then has no guard and presses
    /// nothing.
    #[must_use]
    pub fn header_guard(&self, rows: &[String]) -> String {
        let Some(row) = rows.get(self.header) else {
            return String::new();
        };
        if header_at(row, self.renderer.column()).is_none() {
            return String::new();
        }
        let last = TABS[TABS.len() - 1];
        // The first `Usage` after the title is the tab's: the title's own
        // word is at the row's start, and `header_at` has placed the tabs.
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

/// The Settings panel, where it is what the screen shows. `Some` only when
/// ALL hold:
///
/// * no composer frame is on the rows (the panel replaces the composer, in
///   both renderers);
/// * a full-width rule — `▔` for the fullscreen renderer, `─` for the
///   inline one — whose next non-blank row is the header, at that
///   renderer's column: `Settings` as a word, then `Status`, `Config` and
///   `Usage` named in that order;
/// * every non-blank row below the header is at the header's column or
///   further in.
///
/// With more than one such rule, the LOWEST is the panel's (the live zone is
/// at the bottom; a transcript above it may quote anything).
///
/// What this refuses is the point. A header with no rule over it (a quoted
/// panel, the copied text form); the header one column off (another
/// renderer's, or a quote); the same rows with the composer drawn (the
/// worker pasted a panel into its answer, and the session is at its
/// prompt); anything under the header that starts further left (a shell
/// line, a transcript row: the panel is not the last thing on the screen).
/// A caller that closes "the panel" presses Escape, which elsewhere stops a
/// turn — so no reading here is a guess.
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

/// Whether `row`, a row under the Settings header, is one only the USAGE tab
/// draws: the session block's `Total cost:`, a `Current session` window
/// title, or the loading line. One such row is what tells the tab drawn from
/// Status, Config and Stats.
#[must_use]
pub fn usage_tab_mark(row: &str) -> bool {
    let t = crate::text::trim(row);
    USAGE_MARKS.iter().any(|mark| t.starts_with(mark)) || t.starts_with(anchor("usage.loading"))
}

/// Whether `row`, a row under the Settings header, says the Usage tab's
/// figures are NOT SETTLED: a load or a refresh in flight (`Loading usage
/// data…`, `Refreshing…`), figures the vendor calls last-known, partial or
/// unrefreshed (its enrich-error line), a failed load (`Error: …`), or any
/// row that says `rate limited`. READ in the 2.1.287 bundle (module header).
#[must_use]
pub fn usage_unsettled(row: &str) -> bool {
    let t = crate::text::trim(row);
    UNSETTLED.iter().any(|id| t.starts_with(anchor(id)))
        || t.starts_with(ERROR_HEAD)
        || t.contains(RATE_LIMITED)
}

/// Whether `row` is the panel's TAIL: the cancel hint and nothing else
/// (`Esc to cancel`, the `confirm:no` binding's key and its description —
/// [`anchor`] `prompt.cancel`, a key hint, so a rebound key is another
/// word and no tail), ASCII case ignored. The vendor draws it LAST (READ:
/// module header), so the tail on the grid is what says the whole panel is:
/// `Refreshing…` and the enrich-error line stand right above it and can be
/// under the fold while the windows show. A retry hint beside it (drawn only
/// with an enrich error) is not this row.
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

    /// MEASURED: rows 1 to 10 and 21 to 26 of aterm-agent's
    /// `tests/testdata/vendor/2.1.278/screen/usage.txt` (Claude Code
    /// 2.1.278, 2026-09-22), verbatim — the blank first row, the done row,
    /// the `▔` rule, the header, the head of the session block and the first
    /// window. The whole capture is read by aterm-agent's `harness::usage`
    /// tests. (The literal opens with a real line break and no `\`: a
    /// continuation would eat the first row's indent.)
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

    /// THE PANEL AS MEASURED: under the `▔` rule, the header three columns
    /// in, fullscreen. NEGATIVE CONTROLS, each the same rows with one thing
    /// changed: no rule over the header; the header one column further out,
    /// and one further in; a row under it that starts at column zero; the
    /// composer's frame drawn under it; a tab missing; the tabs out of
    /// order; the title run into another word.
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
        // The composer's frame ABOVE the panel (its rows under it would be
        // refused for their column alone): everything else of the panel is
        // as measured, so this is the frame's own refusal.
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
        // What does NOT matter: the spacing between the tabs, a tab after
        // them, a blank row between the rule and the header, a toast drawn
        // inside the rule.
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

    /// THE PANEL ON EVERY MEASURED 2.1.287 SCREEN (`prompt::fixtures`): the
    /// fullscreen renderer's at five grids — unsettled, settled, with its
    /// tail under the fold, whole on a tall grid, narrow, short — always
    /// under the `▔` rule on the third row with the header on the fourth;
    /// the inline renderer's under its `─` rule, two columns in. NEGATIVE
    /// CONTROLS: the inline panel once the main screen scrolled its rule and
    /// header off the grid is no panel (measured: nothing on that grid
    /// places it); and the two renderers' shapes are not interchangeable — a
    /// `─` rule over a header three columns in, and a `▔` rule over one two
    /// columns in, are no panel.
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
            // What the crate's own reader says of a panel screen: idle by
            // default, with no composer — and its word is no evidence.
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

    /// THE ROWS OF THE MEASURED TABS, told by the predicates the settled
    /// rule is made of: every measured screen holds a mark of the Usage tab;
    /// only the refreshing one holds an unsettled row; the settled, whole,
    /// narrow and inline ones end on the tail, and the folded and short ones
    /// — and the refreshing one's last row IS the tail too, which is why the
    /// unsettled rows are asked first. NEGATIVE CONTROL: `Scanning local
    /// sessions…`, the contributing block's own loading word, is no
    /// unsettled row (it says nothing of the windows).
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
        // The fold's arrow, as measured: ending a row of the block, or alone.
        assert!(last(&fx::screen(fx::USAGE_FOLD)).ends_with('↓'));
        assert_eq!(last(&fx::screen(fx::USAGE_SHORT)).trim(), "↓");
    }

    /// The lowest rule is the panel's: a panel quoted in the transcript
    /// above the live one (its own rule and header, at the panel's column)
    /// does not move the reading. NEGATIVE CONTROL: with the live panel gone
    /// and the quoted one left over a shell line, there is no panel.
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

    /// THE HEADER'S GUARD is the row as observed, anchored at column zero,
    /// one wire token. NEGATIVE CONTROL: rows that do not hold the header
    /// this value names give no guard.
    #[test]
    fn the_header_guard_is_the_observed_row_through_the_usage_tab() {
        let measured = rows(HEAD_2_1_278);
        let panel = settings_panel(&measured).expect("the panel");
        let guard = panel.header_guard(&measured);
        assert_eq!(guard, "^...Settings..Status...Config...Usage");
        assert!(!guard.contains(char::is_whitespace));
        // Another spacing gives another guard: it is the row, not a
        // template.
        let mut spaced = measured.clone();
        spaced[3] = "   Settings   Status  Config  Usage  Stats".to_string();
        assert_eq!(
            panel.header_guard(&spaced),
            "^...Settings...Status..Config..Usage"
        );
        // No header where the value says: no guard.
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

    /// The Usage tab's marks, the unsettled rows and the tail, each by the
    /// vendor's own words. NEGATIVE CONTROLS beside each: the settled
    /// panel's own rows are no unsettled row; the hint with a retry beside
    /// it, or the fold's arrow, is no tail; another tab's rows are no mark.
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
