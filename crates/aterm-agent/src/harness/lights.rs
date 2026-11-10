// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Claude Code light beside aterm's footer: the permission mode, read as one bit
//! (`docs/DESIGN-yolo-binary-2026-11-04.md` §1): yolo where Claude's pill reads bypass, not yolo in
//! any other mode.

use crate::harness::footer;

/// How long an Autopilot chip's click waits for its word to read back before the chip lets it go.
pub const SETTLE_MS: u64 = 2500;

/// One light of the row; the permission mode is the only one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Light {
    /// Claude's permission mode, one bit whatever its pill names: yolo on in bypass
    /// ([`footer::YOLO_ON_MARK`]), off in every other mode ([`footer::YOLO_OFF_MARK`]).
    Mode,
}

impl Light {
    /// Every light, in the order the row draws them.
    pub const ALL: [Light; 1] = [Light::Mode];

    /// The light's name, as an accessibility label names it (`Permission mode: yolo.`); the
    /// glass title says what the state does instead, beside a chip that already shows the
    /// state's word.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Light::Mode => "Permission mode",
        }
    }
}

/// Claude Code's permission mode, as its pill names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    /// `bypass permissions`.
    Bypass,
    /// `auto mode`.
    Auto,
    /// `accept edits`.
    AcceptEdits,
    /// `don't ask`.
    DontAsk,
    /// `plan mode`.
    Plan,
    /// `manual mode` — Claude's default mode.
    Manual,
}

impl Mode {
    /// The anchor id of the mode's pill (`mode.auto`), [`Self::of_indicator`]'s inverse.
    #[must_use]
    pub const fn indicator(self) -> &'static str {
        match self {
            Mode::Bypass => "mode.bypass",
            Mode::Auto => "mode.auto",
            Mode::AcceptEdits => "mode.accept_edits",
            Mode::DontAsk => "mode.dont_ask",
            Mode::Plan => "mode.plan",
            Mode::Manual => "mode.manual",
        }
    }

    /// The mode in Claude Code's own words, as its pill says it (`auto mode on`,
    /// `bypass permissions on`): what the chip's hover names the mode by, so aterm keeps no
    /// vocabulary of its own for a mode.
    #[must_use]
    pub const fn pill_words(self) -> &'static str {
        aterm_phase::anchor_text(self.indicator())
    }

    /// The mode a pill's anchor id (`mode.auto`, [`footer::pill_indicator`]) names.
    #[must_use]
    pub fn of_indicator(id: &str) -> Option<Mode> {
        Some(match id {
            "mode.bypass" => Mode::Bypass,
            "mode.auto" => Mode::Auto,
            "mode.accept_edits" => Mode::AcceptEdits,
            "mode.dont_ask" => Mode::DontAsk,
            "mode.plan" => Mode::Plan,
            "mode.manual" => Mode::Manual,
            _ => return None,
        })
    }
}

/// What one Claude Code screen says about the light's setting and the turn, read off the engine where a click on the light is decided and for a live upgrade's note; a frame folds in [`read_lights`] instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    /// The permission mode its pill names (every mode draws one, default included: `⏸ manual mode on`).
    pub mode: Option<Mode>,
    /// A pill is drawn under the composer, known or not: a row opening with a pill glyph (`footer::opens_with_pill_glyph`), a cut one included.
    pub pill: bool,
    /// Claude is mid-turn (`aterm_phase::phase::busy_seen`, hard): the screen shows the turn, or the window title runs one it does not show ([`Screen::title_live`]).
    pub busy: bool,
    /// The window title runs a turn the screen does not show (`aterm_phase::phase::title_live`: a reply drawn under a status line, whose footer and spinner are gone), the one live turn no background pill excuses.
    pub title_live: bool,
    /// The pill stands after vim INSERT's indicator (`footer::vim_insert`: `  -- INSERT -- ⏵⏵ auto mode on …`, MEASURED on 2.1.304): the pill names its mode as on any mode row, and the composer takes keys through vim.
    pub vim_insert: bool,
}

/// Read `rows` (one pane, top to bottom) with the window title Claude Code set over them (`None` where the caller holds none: then the screen alone says whether a turn runs).
#[must_use]
pub fn read_screen(rows: &[String], title: Option<&str>) -> Option<Screen> {
    use aterm_phase::ScreenReader as _;
    let (_, bottom) = aterm_phase::phase::composer_rules(rows)?;
    let Reading { mode, pill } = reading_under(rows, bottom);
    let runs = aterm_phase::ClaudeReader.runs(title);
    let busy = aterm_phase::phase::busy_seen(rows, runs).is_some_and(|b| !b.soft);
    let title_live = aterm_phase::phase::title_live(rows, runs);
    let vim_insert =
        footer::pill_row_under(rows, bottom).is_some_and(|r| footer::vim_insert(&rows[r]));
    Some(Screen {
        mode,
        pill,
        busy,
        title_live,
        vim_insert,
    })
}

/// What the lights show of `rows`: [`read_screen`]'s mode and pill, without the turn.
#[must_use]
pub fn read_lights(rows: &[String]) -> Option<Reading> {
    let (_, bottom) = aterm_phase::phase::composer_rules(rows)?;
    Some(reading_under(rows, bottom))
}

/// [`read_lights`] given the composer's bottom rule the caller has found (row `bottom`): it reads only the rows under the rule, for the pill with its `(shift+tab to cycle)` hint whole or cut (`footer::pill_row_under`).
#[must_use]
pub fn reading_under(rows: &[String], bottom: usize) -> Reading {
    let row = footer::pill_row_under(rows, bottom);
    Reading {
        mode: row
            .and_then(|r| footer::pill_indicator(&rows[r]))
            .and_then(Mode::of_indicator),
        pill: row.is_some(),
    }
}

/// What the lights show: the permission mode its pill names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Reading {
    /// The permission mode, where a pill this build knows is drawn.
    pub mode: Option<Mode>,
    /// A pill is drawn, known or not ([`Screen::pill`]).
    pub pill: bool,
}

impl Default for Reading {
    fn default() -> Self {
        Self::UNKNOWN
    }
}

impl Reading {
    /// Nothing known: no screen yet, or a box covering the composer.
    pub const UNKNOWN: Reading = Reading {
        mode: None,
        pill: false,
    };

    /// The bit, where a pill this build knows is drawn: `Some(true)` in bypass (yolo), `Some(false)`
    /// in any other mode, `None` with no mode read.
    #[must_use]
    pub fn yolo(&self) -> Option<bool> {
        self.mode.map(|m| m == Mode::Bypass)
    }

    /// Fold a newer screen reading in.
    pub const fn fold(&mut self, newer: &Reading) {
        if newer.mode.is_some() || newer.pill {
            self.mode = newer.mode;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(rule: &str, mode_row: &str) -> Vec<String> {
        vec![
            "\u{25CF} done".into(),
            String::new(),
            rule.into(),
            "\u{276F} ".into(),
            "\u{2500}".repeat(60),
            mode_row.into(),
        ]
    }

    const BYPASS: &str = "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle) \u{00B7} \u{2190} for agents";

    /// The mode shown survives a box (no composer) and a menu (no pill row),
    /// but a drawn pill this build cannot name leaves the bit unread.
    #[test]
    fn a_drawn_pill_this_build_cannot_name_replaces_the_mode_shown() {
        let rule = "\u{2500}".repeat(60);
        let bypass = read_lights(&screen(&rule, BYPASS)).unwrap();
        let fold_from_bypass = |newer: Option<Reading>| {
            let mut shown = bypass;
            shown.fold(&newer.unwrap_or(Reading::UNKNOWN));
            shown.yolo()
        };
        assert_eq!(fold_from_bypass(None), Some(true), "a box keeps it");
        let menu = screen(&rule, "  /model   Set the AI model");
        assert_eq!(
            fold_from_bypass(read_lights(&menu)),
            Some(true),
            "so does a menu"
        );
        for row in ["  \u{23F5}\u{23F5} turbo mode on", "  \u{23F8} plan mo"] {
            let rows = screen(&rule, row);
            let s = read_screen(&rows, None).unwrap();
            assert!(s.pill && s.mode.is_none(), "{row:?}");
            assert_eq!(fold_from_bypass(read_lights(&rows)), None, "{row:?}");
        }
        let plan = screen(&rule, "  \u{23F8} plan mode on");
        assert_eq!(fold_from_bypass(read_lights(&plan)), Some(false));
    }

    /// A pill whose `(shift+tab to cycle)` hint a narrow pane cut, between
    /// words or mid-word, still names the mode.
    #[test]
    fn a_narrow_panes_cut_hint_still_names_the_mode() {
        let rule = "\u{2500}".repeat(38);
        let cases = [
            (
                "  \u{23F5}\u{23F5} bypass permissions on (shift+tab",
                Mode::Bypass,
            ),
            ("  \u{23F5}\u{23F5} auto mode on (shift+tab to", Mode::Auto),
            ("  \u{23F8} plan mode on (shift+tab to", Mode::Plan),
            ("  \u{23F5}\u{23F5} auto mode on (shif", Mode::Auto),
        ];
        for (row, mode) in cases {
            let rows = screen(&rule, row);
            let s = read_screen(&rows, None).expect("a composer");
            assert_eq!(s.mode, Some(mode), "{row:?}");
            assert_eq!(
                read_lights(&rows).and_then(|r| r.yolo()),
                Some(mode == Mode::Bypass),
                "{row:?}"
            );
        }
    }

    /// Every mode, in the order Claude's pill table lists them.
    const ALL_MODES: [Mode; 6] = [
        Mode::Bypass,
        Mode::Auto,
        Mode::AcceptEdits,
        Mode::DontAsk,
        Mode::Plan,
        Mode::Manual,
    ];

    /// A box over the composer: nothing is known, so the bit is unread.
    #[test]
    fn no_composer_no_lights_and_no_drive() {
        assert_eq!(read_screen(&["$ ls".to_owned()], None), None);
        assert_eq!(read_lights(&["$ ls".to_owned()]), None);
        assert_eq!(Reading::UNKNOWN.yolo(), None);
    }

    /// EVERY MODE HAS ONE PILL (`lights-mode-pill-table`): each mode is named by exactly one entry
    /// of the footer's pill table and each entry names a mode, so the bit, the bypass row's
    /// rewrite and a restart's carried mode (`relaunch::screen_mode`) read every pill Claude draws.
    #[test]
    fn every_mode_has_one_pill_and_every_pill_a_mode() {
        for mode in ALL_MODES {
            let pills = footer::PILLS
                .iter()
                .filter(|pill| Mode::of_indicator(pill.id) == Some(mode))
                .count();
            assert_eq!(pills, 1, "{mode:?}");
        }
        for pill in &footer::PILLS {
            assert!(Mode::of_indicator(pill.id).is_some(), "{}", pill.id);
        }
        for mode in ALL_MODES {
            assert_eq!(Mode::of_indicator(mode.indicator()), Some(mode), "{mode:?}");
            assert!(
                mode.pill_words().ends_with(" on"),
                "{mode:?}: the pill's own words"
            );
        }
        assert_eq!(Mode::Plan.pill_words(), "plan mode on");
        // The chip says one word either way, and its mark says which (owner, 2026-11-09): the
        // off chip never wears another word for the flag.
        assert_eq!(
            (footer::YOLO, footer::YOLO_ON_MARK, footer::YOLO_OFF_MARK),
            ("yolo", "\u{25cf}", "\u{25cb}")
        );
        assert_eq!(
            (footer::YOLO_ON_SAID, footer::YOLO_OFF_SAID),
            ("yolo on", "yolo off")
        );
    }

    /// THE BIT IS BYPASS ALONE (`docs/DESIGN-yolo-binary-2026-11-04.md` §1): yolo where the pill
    /// reads bypass, not yolo in every other mode, `auto` included.
    #[test]
    fn the_bit_is_yolo_in_bypass_alone() {
        for mode in ALL_MODES {
            let at = Reading {
                mode: Some(mode),
                pill: true,
            };
            assert_eq!(at.yolo(), Some(mode == Mode::Bypass), "{mode:?}");
        }
    }

    /// Every recorded Claude Code screen in `aterm-phase`'s fixtures reads a
    /// definite state for every light it shows, or (a box up) no screen at all.
    #[test]
    fn every_recorded_screen_reads_definitely_or_not_at_all() {
        let dir = aterm_tempfile::manifest_dir().join("../aterm-phase/src/fixtures");
        let mut readable = 0;
        for entry in std::fs::read_dir(&dir).expect("the phase fixtures") {
            let path = entry.unwrap().path();
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let rows: Vec<String> = text.lines().map(str::to_owned).collect();
            let Some(s) = read_screen(&rows, None) else {
                continue;
            };
            let got = read_lights(&rows).expect("a composer, as read_screen saw");
            let pill = footer::mode_row(&rows).is_some();
            assert!(got.yolo().is_some() == pill, "{}: {got:?}", path.display());
            // The prompt-history search (Ctrl-R) heads the mode row with its words and query
            // (measured on 2.1.318): no mode row is read there, so the glass leaves the row whole.
            if rows
                .iter()
                .any(|r| r.contains("\u{23F5}\u{23F5} auto mode on"))
                && !aterm_phase::phase::history_search(&rows)
            {
                assert_eq!(s.mode, Some(Mode::Auto), "{}", path.display());
            }
            readable += 1;
        }
        assert!(
            readable >= 10,
            "the corpus must exercise the reader: {readable}"
        );
    }

    /// On every recorded screen in `aterm-phase`'s fixtures, [`read_lights`] is
    /// [`read_screen`]'s mode and pill, and [`reading_under`] the composer's bottom
    /// rule gives the same reading.
    #[test]
    fn the_lights_reading_is_the_screens_without_its_turn() {
        let dir = aterm_tempfile::manifest_dir().join("../aterm-phase/src/fixtures");
        let (mut framed, mut bare) = (0, 0);
        for entry in std::fs::read_dir(&dir).expect("the phase fixtures") {
            let path = entry.expect("a fixture").path();
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let rows: Vec<String> = text.lines().map(str::to_owned).collect();
            let screen = read_screen(&rows, None);
            let reading = read_lights(&rows);
            assert_eq!(
                reading,
                screen.as_ref().map(|s| Reading {
                    mode: s.mode,
                    pill: s.pill
                }),
                "{}",
                path.display()
            );
            match aterm_phase::phase::composer_rules(&rows) {
                Some((_, bottom)) => {
                    assert_eq!(reading, Some(reading_under(&rows, bottom)));
                    framed += 1;
                }
                None => bare += 1,
            }
        }
        assert!(
            framed > 10 && bare > 10,
            "CONTROL: {framed} framed, {bare} not"
        );
    }

    /// The drift canary: every recorded footer screen of every Claude Code
    /// build (`claude-<version>-footer-*.txt` in aterm-phase's fixtures, from
    /// `aterm-lab footers`) has a composer, a mode row under it and a pill read
    /// as a mode.
    #[test]
    fn every_captured_footer_screen_of_every_build_is_read() {
        let dir = aterm_tempfile::manifest_dir().join("../aterm-phase/src/fixtures");
        let mut builds = std::collections::BTreeSet::new();
        let mut screens = 0;
        for entry in std::fs::read_dir(&dir).expect("the phase fixtures") {
            let path = entry.expect("a fixture").path();
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_owned();
            let Some(version) = name
                .strip_prefix("claude-")
                .and_then(|rest| rest.split_once("-footer-"))
                .map(|(version, _)| version.to_owned())
            else {
                continue;
            };
            let text = std::fs::read_to_string(&path).expect("a readable fixture");
            let rows: Vec<String> = text
                .lines()
                .skip_while(|line| line.starts_with('#'))
                .map(str::to_owned)
                .collect();
            let screen = read_screen(&rows, None)
                .unwrap_or_else(|| panic!("{name}: no composer this build's reader sees"));
            let row = footer::mode_row(&rows)
                .unwrap_or_else(|| panic!("{name}: no mode row the lights read"));
            assert!(
                footer::pill_indicator(&rows[row]).is_some(),
                "{name}: the mode pill is not one this build reads: {:?}",
                rows[row]
            );
            assert!(
                screen.mode.is_some(),
                "{name}: the lights read no mode from {:?}",
                rows[row]
            );
            builds.insert(version);
            screens += 1;
        }
        assert!(
            screens >= 8 && builds.contains("2.1.283"),
            "the canary must judge the measured corpus: {screens} screens of {builds:?}"
        );
    }

    /// THE 2.1.301 FOOTERS (measured by `aterm-lab footers`: the real
    /// Claude Code in the lab's private headless aterm, a subscriber login,
    /// model opus at the top effort level): each mode the launch's shift+tab
    /// cycle shows reads as that mode, idle, with the mode row under the rule
    /// the footer writes into — the effort tag drawn at the row's right end
    /// (`◉ <level> · /effort`) included.
    #[test]
    fn the_2_1_301_screens_read_as_measured() {
        let cases: [(&str, &str, Mode); 5] = [
            (
                "auto-idle",
                include_str!(
                    "../../../aterm-phase/src/fixtures/claude-2.1.301-footer-auto-idle.txt"
                ),
                Mode::Auto,
            ),
            (
                "auto-draft",
                include_str!(
                    "../../../aterm-phase/src/fixtures/claude-2.1.301-footer-auto-draft.txt"
                ),
                Mode::Auto,
            ),
            (
                "manual",
                include_str!("../../../aterm-phase/src/fixtures/claude-2.1.301-footer-manual.txt"),
                Mode::Manual,
            ),
            (
                "accept-edits",
                include_str!(
                    "../../../aterm-phase/src/fixtures/claude-2.1.301-footer-accept-edits.txt"
                ),
                Mode::AcceptEdits,
            ),
            (
                "plan",
                include_str!("../../../aterm-phase/src/fixtures/claude-2.1.301-footer-plan.txt"),
                Mode::Plan,
            ),
        ];
        for (name, text, mode) in cases {
            let rows: Vec<String> = text.lines().skip(1).map(str::to_owned).collect();
            let s = read_screen(&rows, None).unwrap_or_else(|| panic!("{name}: a composer"));
            assert_eq!(s.mode, Some(mode), "{name}");
            assert!(!s.busy, "{name}: idle");
            let rule = aterm_phase::phase::composer_bottom(&rows)
                .unwrap_or_else(|| panic!("{name}: the composer's bottom rule"));
            assert!(
                footer::mode_row(&rows).is_some_and(|r| r > rule),
                "{name}: the mode row is read under the rule the footer writes into"
            );
        }
    }

    /// THE 2.1.307 AND 2.1.308 FOOTERS (measured by `aterm-lab footers` in the
    /// lab's private headless aterm at 80x24, model opus at the top effort
    /// level; docs/measured/claude-code-2.1.307-2026-10-21.md and
    /// docs/measured/claude-code-2.1.308-2026-10-21.md): each reads as the
    /// mode and turn it was captured in, the mode row under the composer's
    /// rule, with the vendor's `← for agents` hint gone from the rows drawn
    /// past its first 10 s and the bypass row's effort tag wrapped onto a row
    /// of its own.
    #[test]
    fn the_lab_footer_screens_read_as_measured() {
        let cases: [(&str, Mode, bool); 8] = [
            ("auto-idle", Mode::Auto, false),
            ("auto-draft", Mode::Auto, false),
            ("manual", Mode::Manual, false),
            ("accept-edits", Mode::AcceptEdits, false),
            ("plan", Mode::Plan, false),
            ("busy", Mode::Auto, true),
            ("after-turn", Mode::Auto, false),
            ("bypass-idle", Mode::Bypass, false),
        ];
        let dir = aterm_tempfile::manifest_dir().join("../aterm-phase/src/fixtures");
        for build in ["2.1.307", "2.1.308"] {
            for (name, mode, busy) in cases {
                let file = format!("claude-{build}-footer-{name}.txt");
                let text = std::fs::read_to_string(dir.join(&file))
                    .unwrap_or_else(|e| panic!("{file}: {e}"));
                let rows: Vec<String> = text.lines().skip(1).map(str::to_owned).collect();
                let s = read_screen(&rows, None).unwrap_or_else(|| panic!("{file}: a composer"));
                assert_eq!(s.mode, Some(mode), "{file}");
                assert_eq!(s.busy, busy, "{file}");
                let rule = aterm_phase::phase::composer_bottom(&rows)
                    .unwrap_or_else(|| panic!("{file}: the composer's bottom rule"));
                let row = footer::mode_row(&rows)
                    .filter(|r| *r > rule)
                    .unwrap_or_else(|| panic!("{file}: the mode row under the rule"));
                let mut blank = rows.clone();
                blank[row].clear();
                assert_eq!(
                    read_screen(&blank, None).and_then(|s| s.mode),
                    None,
                    "{file}: CONTROL"
                );
            }
        }
    }

    /// Every 2.1.283 capture (`aterm ctl text` of a private headless aterm)
    /// reads as the mode and turn state it was captured in.
    #[test]
    fn the_2_1_283_screens_read_as_measured() {
        let cases: [(&str, &str, Mode, bool); 8] = [
            (
                "auto-idle",
                include_str!(
                    "../../../aterm-phase/src/fixtures/claude-2.1.283-footer-auto-idle.txt"
                ),
                Mode::Auto,
                false,
            ),
            (
                "auto-draft",
                include_str!(
                    "../../../aterm-phase/src/fixtures/claude-2.1.283-footer-auto-draft.txt"
                ),
                Mode::Auto,
                false,
            ),
            (
                "manual",
                include_str!("../../../aterm-phase/src/fixtures/claude-2.1.283-footer-manual.txt"),
                Mode::Manual,
                false,
            ),
            (
                "accept-edits",
                include_str!(
                    "../../../aterm-phase/src/fixtures/claude-2.1.283-footer-accept-edits.txt"
                ),
                Mode::AcceptEdits,
                false,
            ),
            (
                "plan",
                include_str!("../../../aterm-phase/src/fixtures/claude-2.1.283-footer-plan.txt"),
                Mode::Plan,
                false,
            ),
            (
                "busy",
                include_str!("../../../aterm-phase/src/fixtures/claude-2.1.283-footer-busy.txt"),
                Mode::Auto,
                true,
            ),
            (
                "after-turn",
                include_str!(
                    "../../../aterm-phase/src/fixtures/claude-2.1.283-footer-after-turn.txt"
                ),
                Mode::Auto,
                false,
            ),
            (
                "bypass-idle",
                include_str!(
                    "../../../aterm-phase/src/fixtures/claude-2.1.283-footer-bypass-idle.txt"
                ),
                Mode::Bypass,
                false,
            ),
        ];
        for (name, text, mode, busy) in cases {
            let rows: Vec<String> = text.lines().skip(1).map(str::to_owned).collect();
            let s = read_screen(&rows, None).unwrap_or_else(|| panic!("{name}: a composer"));
            assert_eq!(s.mode, Some(mode), "{name}");
            assert_eq!(s.busy, busy, "{name}");
            let rule = aterm_phase::phase::composer_bottom(&rows)
                .unwrap_or_else(|| panic!("{name}: the composer's bottom rule"));
            assert!(
                footer::mode_row(&rows).is_some_and(|r| r > rule),
                "{name}: the mode row is read under the rule the footer writes into"
            );
        }
    }

    /// A reply drawn under a status line shows no busy sign (2.1.311 lab `mr`, MEASURED): the
    /// window title's `◐`/`◑` is the turn a restart waits out, while the pill still
    /// names auto mode. A title at rest, one with no glyph, or none held reads the screen alone
    /// (not busy), and the done row ends the turn whatever the title says.
    #[test]
    fn a_reply_drawn_under_a_status_line_is_busy_while_its_title_runs() {
        use aterm_phase::prompt::fixtures::{
            STATUSLINE_LAST_LINE_HELD_2_1_311, STATUSLINE_MIDREPLY_2_1_311,
            STATUSLINE_REPLY_DONE_2_1_311, screen as rows_of,
        };
        let mid = rows_of(STATUSLINE_MIDREPLY_2_1_311);
        let held = rows_of(STATUSLINE_LAST_LINE_HELD_2_1_311);
        let done = rows_of(STATUSLINE_REPLY_DONE_2_1_311);
        for (name, rows) in [("mid-reply", &mid), ("last line held", &held)] {
            for title in ["\u{25D0} Claude Code", "\u{25D1} Claude Code"] {
                let s = read_screen(rows, Some(title)).unwrap_or_else(|| panic!("{name}"));
                assert!(s.busy && s.title_live, "{name} under {title:?}: {s:?}");
                assert_eq!(s.mode, Some(Mode::Auto), "{name}");
            }
            for title in [
                Some("\u{2733} Claude Code"),
                Some("Claude Code"),
                Some(""),
                None,
            ] {
                let s = read_screen(rows, title).unwrap_or_else(|| panic!("{name}"));
                assert!(!s.busy && !s.title_live, "{name} under {title:?}: {s:?}");
                assert_eq!(s.mode, Some(Mode::Auto), "{name}");
            }
        }
        let s = read_screen(&done, Some("\u{25D0} Claude Code")).expect("the done frame");
        assert!(
            !s.busy && !s.title_live,
            "the done row ends the turn: {s:?}"
        );
    }
}
