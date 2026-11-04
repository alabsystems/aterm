// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Claude Code light beside aterm's footer: the permission mode, the one setting the owner always expects on.

use crate::harness::footer;

/// How long one shift+tab of a mode return may take to show on Claude's screen before the host calls it failed.
pub const SETTLE_MS: u64 = 2500;

/// How many shift+tab presses a mode toggle may spend on its way to its [`Goal`].
pub const MAX_MODE_PRESSES: u8 = 5;

/// One light of the row; the permission mode is the only one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Light {
    /// Claude's permission mode, one setting whatever its pill names: on in an expected mode ([`Mode::is_expected`]: bypass or auto), off otherwise.
    Mode,
}

impl Light {
    /// Every light, in the order the row draws them.
    pub const ALL: [Light; 1] = [Light::Mode];

    /// The light's word on its chip while its state is not known; the mode light's chip names its mode ([`Mode::word`]) once one is.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Light::Mode => "mode",
        }
    }

    /// The light's name, as an accessibility label names it (`Permission mode: yolo.`); the
    /// glass title says what the state does instead ([`Mode::gist`]), beside a chip that already
    /// shows the state's word.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Light::Mode => "Permission mode",
        }
    }
}

/// Where a click on a light sends it: the state it does not show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Goal {
    /// Back to an expected mode (bypass or auto): the click on an Off light.
    On,
    /// To manual mode (`lame` on a rewritten row): the click on an On light.
    Off,
}

impl Goal {
    /// The goal a click on the light showing `mode` has, the state it does not show:
    /// [`Goal::Off`] from an expected mode (the light On), [`Goal::On`] from any other.
    #[must_use]
    pub const fn of_mode(mode: Mode) -> Goal {
        if mode.is_expected() {
            Goal::Off
        } else {
            Goal::On
        }
    }

    /// Whether `mode` is where the toggle is going: an expected mode for [`Goal::On`], manual
    /// mode for [`Goal::Off`].
    #[must_use]
    pub const fn reached(self, mode: Mode) -> bool {
        match self {
            Goal::On => mode.is_expected(),
            Goal::Off => matches!(mode, Mode::Manual),
        }
    }

    /// Where it goes, as a title's click clause names it (`yolo or auto`, `lame`; on Claude's own
    /// row `bypass or auto`, `manual`).
    #[must_use]
    pub fn words(self, rewritten: bool) -> String {
        match self {
            Goal::On => format!("{} or auto", Mode::Bypass.word_on_glass(rewritten)),
            Goal::Off => Mode::Manual.word_on_glass(rewritten).to_owned(),
        }
    }
}

/// The ink a mode's chip is drawn in ([`Mode::ink`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModeInk {
    /// The ink of the composer's bottom rule the chip sits on: bypass, the default, named quietly.
    Rule,
    /// The mode's own hue, as Claude's pill wears it (auto's warning amber).
    Hue([u8; 3]),
    /// The row's plain ink: a mode the owner does not expect, drawn as plainly as Claude's text.
    Row,
}

/// What a light shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LightState {
    /// The setting is on.
    On,
    /// The setting is off.
    Off,
    /// No screen has shown it yet, or the pill drawn is one this module does not know (a box over the composer keeps the last state shown).
    Unknown,
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

    /// Whether the owner expects this mode: bypass (auto-approve) or auto.
    #[must_use]
    pub const fn is_expected(self) -> bool {
        matches!(self, Mode::Bypass | Mode::Auto)
    }

    /// The mode's short word, as the light names it: `bypass`, `auto`, `accept edits`, `don't ask`, `plan`, `manual`.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Mode::Bypass => "bypass",
            Mode::Auto => "auto",
            Mode::AcceptEdits => "accept edits",
            Mode::DontAsk => "don't ask",
            Mode::Plan => "plan",
            Mode::Manual => "manual",
        }
    }

    /// The mode's word where the glass rewrites Claude's mode row (`rewritten`: the host dropped the pill this frame, under its gate and only where the glass took it, [`footer::mode_row_rewrite_under`]), so the lights block is the mode's one home: bypass is [`footer::YOLO_ON`] and manual mode [`footer::YOLO_OFF`]; one state has one name on screen.
    #[must_use]
    pub const fn word_on_glass(self, rewritten: bool) -> &'static str {
        match self {
            Mode::Bypass if rewritten => footer::YOLO_ON,
            Mode::Manual if rewritten => footer::YOLO_OFF,
            _ => self.word(),
        }
    }

    /// The glyph the light draws before its word: none on a rewritten row for the two ends of a
    /// click, an expected mode and manual (`yolo`, `auto`, `lame`), so the toggle between them is a
    /// word for a word; Claude's pill glyph ([`Self::glyph`]) everywhere else (`⏸ plan`).
    #[must_use]
    pub fn glyph_on_glass(self, rewritten: bool) -> &'static str {
        if rewritten && (self.is_expected() || matches!(self, Mode::Manual)) {
            ""
        } else {
            self.glyph()
        }
    }

    /// How a title names the mode as a place: its word on glass and ` mode` (`stopped in plan mode`, `stopped in lame mode`, [`Self::word_on_glass`]).
    #[must_use]
    pub fn named_on_glass(self, rewritten: bool) -> String {
        format!("{} mode", self.word_on_glass(rewritten))
    }

    /// What the mode does, as the light's title says it first: a phrase with no ` — ` or ` · `,
    /// so a title cut to its head ([`Self::claude_name`] and the click clause follow it) is this.
    #[must_use]
    pub const fn gist(self) -> &'static str {
        match self {
            Mode::Bypass => "no permission prompts",
            Mode::Auto => "a classifier answers prompts",
            Mode::AcceptEdits => "edits without asking",
            Mode::DontAsk => "denies unapproved",
            Mode::Plan => "plans before it acts",
            Mode::Manual => "asks before each tool",
        }
    }

    /// Claude Code's own name for the mode: its pill's words without the trailing ` on`
    /// (`bypass permissions`, `manual mode`), which a title gives where the chip wears aterm's word
    /// for it instead (`yolo`, `lame`).
    #[must_use]
    pub fn claude_name(self) -> &'static str {
        footer::PILLS
            .iter()
            .find(|pill| Mode::of_indicator(pill.id) == Some(self))
            .map_or("", |pill| {
                let words = aterm_phase::anchor(pill.id);
                words.strip_suffix(" on").unwrap_or(words)
            })
    }

    /// The pill glyph Claude draws before the mode, from the vendor's table (`footer::PILLS`): `⏸` for plan and manual, `⏵⏵` for the rest.
    #[must_use]
    pub fn glyph(self) -> &'static str {
        footer::PILLS
            .iter()
            .find(|pill| Mode::of_indicator(pill.id) == Some(self))
            .map_or("", |pill| pill.glyph)
    }

    /// The ink the mode's chip is drawn in: bypass in the rule's own ink (`yolo` is the default
    /// and reads as quietly as the rule it sits on, never Claude's red; the owner, 2026-11-02),
    /// auto in its pill's warning amber, every other mode in the row's plain ink.
    #[must_use]
    pub const fn ink(self) -> ModeInk {
        match self {
            Mode::Bypass => ModeInk::Rule,
            Mode::Auto => ModeInk::Hue([0xE8, 0xB0, 0x3C]),
            _ => ModeInk::Row,
        }
    }
}

/// What one Claude Code screen says about the light's setting and the turn, read off the engine where a toggle's next press is decided and for a live upgrade's note; a frame folds in [`read_lights`] instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    /// The permission mode its pill names (every mode draws one, default included: `⏸ manual mode on`).
    pub mode: Option<Mode>,
    /// A pill is drawn under the composer, known or not: a row opening with a pill glyph (`footer::opens_with_pill_glyph`), a cut one included.
    pub pill: bool,
    /// Claude is mid-turn (`aterm_phase::phase::busy_signal`, hard).
    pub busy: bool,
    /// The pill stands after vim INSERT's indicator (`footer::vim_insert`: `  -- INSERT -- ⏵⏵ auto mode on …`, MEASURED on 2.1.304): the pill names its mode as on any mode row, and the composer takes keys through vim.
    pub vim_insert: bool,
}

/// Read `rows` (one pane, top to bottom).
#[must_use]
pub fn read_screen(rows: &[String]) -> Option<Screen> {
    let (_, bottom) = aterm_phase::phase::composer_rules(rows)?;
    let Reading { mode, pill } = reading_under(rows, bottom);
    let busy = aterm_phase::phase::busy_signal(rows).is_some_and(|b| !b.soft);
    let vim_insert =
        footer::pill_row_under(rows, bottom).is_some_and(|r| footer::vim_insert(&rows[r]));
    Some(Screen {
        mode,
        pill,
        busy,
        vim_insert,
    })
}

/// What the lights show of `rows`: [`read`] of [`read_screen`], without the turn.
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

    /// `light`'s state: the mode light is On in an expected mode ([`Mode::is_expected`]) and Off in any other.
    #[must_use]
    pub fn state(&self, light: Light) -> LightState {
        match light {
            Light::Mode => self.mode.map_or(LightState::Unknown, |m| {
                if m.is_expected() {
                    LightState::On
                } else {
                    LightState::Off
                }
            }),
        }
    }

    /// Fold a newer screen reading in.
    pub const fn fold(&mut self, newer: &Reading) {
        if newer.mode.is_some() || newer.pill {
            self.mode = newer.mode;
        }
    }
}

/// Whether `light` shows something the owner does not expect: the reason a light is drawn at rest on Claude's own row (on a row the glass rewrites every known mode is drawn, the light being the mode's one home); a click acts in either state ([`Goal::of_mode`]).
#[must_use]
pub fn deviates(light: Light, reading: &Reading) -> bool {
    reading.state(light) == LightState::Off
}

/// What `screen` says of each light; [`Reading::UNKNOWN`] without one.
#[must_use]
pub fn read(screen: Option<&Screen>) -> Reading {
    screen.map_or(Reading::UNKNOWN, |s| Reading {
        mode: s.mode,
        pill: s.pill,
    })
}

/// shift+tab, as a terminal sends it: the mode light's one input, pressed until its [`Goal`].
pub const SHIFT_TAB: &[u8] = b"\x1b[Z";

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
    /// but a drawn pill this build cannot name turns it Unknown.
    #[test]
    fn a_drawn_pill_this_build_cannot_name_replaces_the_mode_shown() {
        let rule = "\u{2500}".repeat(60);
        let bypass = read_screen(&screen(&rule, BYPASS)).unwrap();
        let fold_from_bypass = |newer: Option<&Screen>| {
            let mut shown = read(Some(&bypass));
            shown.fold(&read(newer));
            shown.state(Light::Mode)
        };
        assert_eq!(fold_from_bypass(None), LightState::On, "a box keeps it");
        let menu = read_screen(&screen(&rule, "  /model   Set the AI model")).unwrap();
        assert_eq!(
            fold_from_bypass(Some(&menu)),
            LightState::On,
            "so does a menu"
        );
        for row in ["  \u{23F5}\u{23F5} turbo mode on", "  \u{23F8} plan mo"] {
            let s = read_screen(&screen(&rule, row)).unwrap();
            assert!(s.pill && s.mode.is_none(), "{row:?}");
            assert_eq!(fold_from_bypass(Some(&s)), LightState::Unknown, "{row:?}");
        }
        let plan = read_screen(&screen(&rule, "  \u{23F8} plan mode on")).unwrap();
        assert_eq!(fold_from_bypass(Some(&plan)), LightState::Off);
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
            let s = read_screen(&screen(&rule, row)).expect("a composer");
            assert_eq!(s.mode, Some(mode), "{row:?}");
            let r = read(Some(&s));
            assert_eq!(deviates(Light::Mode, &r), !mode.is_expected(), "{row:?}");
        }
    }

    /// THE LIGHT IS THE MODE'S ONE NAME (review C8): where the glass rewrites the
    /// mode row bypass reads `yolo` and manual `lame`, each with no glyph, and a title
    /// names every mode as its word and ` mode` (`stopped in lame mode`).
    #[test]
    fn the_lights_word_follows_the_rewritten_row_and_only_under_its_gate() {
        assert_eq!(Mode::Bypass.word_on_glass(true), footer::YOLO_ON);
        assert_eq!(Mode::Bypass.word_on_glass(true), "yolo");
        assert_eq!(Mode::Manual.word_on_glass(true), footer::YOLO_OFF);
        assert_eq!(Mode::Manual.word_on_glass(true), "lame");
        assert_eq!(Mode::Manual.named_on_glass(true), "lame mode");
        assert_eq!(Mode::Bypass.word_on_glass(false), "bypass");
        assert_eq!(Mode::Manual.word_on_glass(false), "manual");
        assert_eq!(Mode::Manual.named_on_glass(false), "manual mode");
        for mode in [Mode::Auto, Mode::AcceptEdits, Mode::DontAsk, Mode::Plan] {
            for rewritten in [true, false] {
                assert_eq!(mode.word_on_glass(rewritten), mode.word(), "{mode:?}");
            }
        }
        for mode in ALL_MODES {
            for rewritten in [true, false] {
                assert_eq!(
                    mode.named_on_glass(rewritten),
                    format!("{} mode", mode.word_on_glass(rewritten)),
                    "{mode:?}"
                );
            }
            let bare = mode.is_expected() || mode == Mode::Manual;
            assert_eq!(mode.glyph_on_glass(true).is_empty(), bare, "{mode:?}");
            assert_eq!(mode.glyph_on_glass(false), mode.glyph(), "{mode:?}");
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

    /// WHAT THE LIGHT'S TITLE SAYS: each mode's gist is a phrase a title can be cut to (no
    /// ` — `, no ` · `), Claude's own name is its pill's words without ` on`, and the click's
    /// goal is the state the light does not show: yolo and auto go to manual (`lame`), every
    /// other mode back to yolo or auto.
    #[test]
    fn each_mode_says_what_it_does_and_where_a_click_takes_it() {
        for mode in ALL_MODES {
            let gist = mode.gist();
            assert!(
                !gist.is_empty() && !gist.contains(" \u{2014} ") && !gist.contains(" \u{b7} "),
                "{mode:?}: {gist:?}"
            );
            let id = footer::PILLS
                .iter()
                .find(|pill| Mode::of_indicator(pill.id) == Some(mode))
                .expect("a pill")
                .id;
            assert_eq!(
                format!("{} on", mode.claude_name()),
                aterm_phase::anchor(id),
                "{mode:?}"
            );
            let at = Reading {
                mode: Some(mode),
                pill: true,
            };
            let goal = Goal::of_mode(mode);
            assert_eq!(
                goal == Goal::Off,
                at.state(Light::Mode) == LightState::On,
                "{mode:?}"
            );
            assert!(
                !goal.reached(mode),
                "{mode:?}: a click always moves the mode"
            );
        }
        assert_eq!(Mode::Bypass.claude_name(), "bypass permissions");
        assert_eq!(Mode::Manual.claude_name(), "manual mode");
        assert!(Goal::Off.reached(Mode::Manual));
        assert!(Goal::On.reached(Mode::Bypass) && Goal::On.reached(Mode::Auto));
        assert_eq!(Goal::On.words(true), "yolo or auto");
        assert_eq!(Goal::Off.words(true), "lame");
        assert_eq!(Goal::On.words(false), "bypass or auto");
        assert_eq!(Goal::Off.words(false), "manual");
    }

    /// YOLO IS DRAWN IN THE RULE'S INK, NEVER RED (the owner, 2026-11-02): bypass takes the
    /// rule's own ink, auto keeps its pill's amber, every other mode the row's plain ink.
    #[test]
    fn yolo_wears_the_rules_ink_and_auto_its_amber() {
        assert_eq!(Mode::Bypass.ink(), ModeInk::Rule);
        assert_eq!(Mode::Auto.ink(), ModeInk::Hue([0xE8, 0xB0, 0x3C]));
        for mode in ALL_MODES {
            assert_eq!(
                matches!(mode.ink(), ModeInk::Row),
                !mode.is_expected(),
                "{mode:?}"
            );
        }
    }

    /// A box over the composer: nothing is known, so nothing deviates.
    #[test]
    fn no_composer_no_lights_and_no_drive() {
        assert_eq!(read_screen(&["$ ls".to_owned()]), None);
        assert_eq!(read(None), Reading::UNKNOWN);
        for light in Light::ALL {
            assert!(!deviates(light, &Reading::UNKNOWN));
        }
    }

    /// The mode light deviates in every mode but bypass and auto (on Claude's own row, the reason
    /// it is drawn at rest).
    #[test]
    fn only_a_mode_the_owner_does_not_expect_deviates() {
        let at = |mode: Mode| Reading {
            mode: Some(mode),
            pill: true,
        };
        for mode in [Mode::Manual, Mode::AcceptEdits, Mode::Plan, Mode::DontAsk] {
            assert!(deviates(Light::Mode, &at(mode)), "{mode:?}");
        }
        for mode in [Mode::Bypass, Mode::Auto] {
            assert!(!deviates(Light::Mode, &at(mode)), "{mode:?}: expected");
        }
    }

    /// Every recorded Claude Code screen in `aterm-phase`'s fixtures reads a
    /// definite state for every light it shows, or (a box up) no screen at all.
    #[test]
    fn every_recorded_screen_reads_definitely_or_not_at_all() {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../aterm-phase/src/fixtures");
        let mut readable = 0;
        for entry in std::fs::read_dir(&dir).expect("the phase fixtures") {
            let path = entry.unwrap().path();
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let rows: Vec<String> = text.lines().map(str::to_owned).collect();
            let Some(s) = read_screen(&rows) else {
                continue;
            };
            let got = read(Some(&s));
            let pill = footer::mode_row(&rows).is_some();
            assert!(
                (got.state(Light::Mode) != LightState::Unknown) == pill,
                "{}: {got:?}",
                path.display()
            );
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
    /// [`read`] of [`read_screen`], and [`reading_under`] the composer's bottom
    /// rule gives the same reading.
    #[test]
    fn the_lights_reading_is_the_screens_without_its_turn() {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../aterm-phase/src/fixtures");
        let (mut framed, mut bare) = (0, 0);
        for entry in std::fs::read_dir(&dir).expect("the phase fixtures") {
            let path = entry.expect("a fixture").path();
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let rows: Vec<String> = text.lines().map(str::to_owned).collect();
            let screen = read_screen(&rows);
            let reading = read_lights(&rows);
            assert_eq!(
                reading,
                screen.as_ref().map(|s| read(Some(s))),
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
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../aterm-phase/src/fixtures");
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
            let screen = read_screen(&rows)
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
            let s = read_screen(&rows).unwrap_or_else(|| panic!("{name}: a composer"));
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
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../aterm-phase/src/fixtures");
        for build in ["2.1.307", "2.1.308"] {
            for (name, mode, busy) in cases {
                let file = format!("claude-{build}-footer-{name}.txt");
                let text = std::fs::read_to_string(dir.join(&file))
                    .unwrap_or_else(|e| panic!("{file}: {e}"));
                let rows: Vec<String> = text.lines().skip(1).map(str::to_owned).collect();
                let s = read_screen(&rows).unwrap_or_else(|| panic!("{file}: a composer"));
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
                    read_screen(&blank).and_then(|s| s.mode),
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
            let s = read_screen(&rows).unwrap_or_else(|| panic!("{name}: a composer"));
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
}
