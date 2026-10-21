// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Claude Code light beside aterm's footer: the permission mode, the one
//! setting the owner always expects on. Claude Code draws its own effort tag
//! and fast mode's `↯`, and thinking never changes, so none of them has a light.
//!
//! The light is drawn only while the mode differs from what the owner expects
//! ([`deviates`]), while a toggle is in flight, or while its answer is on show;
//! at rest nothing is drawn and Claude's own rows are whole. Bypass and auto are
//! both expected ([`Mode::is_expected`]): from any other mode the light's one
//! action presses forward through Claude's cycle to the nearest of them, never
//! away from one.
//!
//! THE OVERRIDE, FOR A MARK THAT IS NOT A LIGHT (owner, 2026-10-15: "I need
//! some visual indicator that the claude code session is being wrapped and a
//! way to toggle on and off to show the improvement";
//! `docs/DESIGN-harness-autopilot-2026-10-15.md` §2.3). The host draws the
//! session's Autopilot chip beside the light AT REST, whenever its footer
//! draws and a harness host exists; no settings button stands beside it
//! (owner, 2026-10-20: "remove the harness settings button in the UX"). It is
//! aterm's own state, read from nothing Claude Code draws, so it is the
//! host's chip and never a [`Light`]; the permission-mode light STAYS
//! DEVIATIONS ONLY.
//!
//! aterm keeps no copy of the vendor's setting that could disagree with it: the
//! light is read from the mode pill (`footer::PILLS`) and toggled by shift+tab;
//! aterm's own presses after the click go only while Claude is idle. A pill this
//! build cannot name (renamed, or cut by a narrow pane) reads
//! [`LightState::Unknown`] and cannot be clicked; a box or menu keeps the last
//! mode read ([`Reading::fold`]). Each press must be answered within
//! [`SETTLE_MS`], and the next goes only on an answer read, never on a timer: a
//! renamed pill greys the light out and can never make aterm press keys into a
//! screen it no longer understands.

use crate::harness::footer;

/// How long one shift+tab of a mode return may take to show on Claude's screen
/// before the host calls it failed. A ceiling: a pill redraw lands within a
/// frame or two.
pub const SETTLE_MS: u64 = 2500;

/// How many shift+tab presses a mode toggle may spend on its way back to an
/// expected mode. Claude's cycle (2.1.283) is default → accept edits → plan →
/// bypass (when the session allows it) → auto (when available) → default;
/// don't ask leads on to default. The nearest expected mode is at most three
/// presses from manual, four from don't ask. The host also stops a search
/// when the cycle comes round to a mode it has already shown, naming the mode
/// it stopped in; this bound is the backstop.
pub const MAX_MODE_PRESSES: u8 = 5;

/// One light of the row; the permission mode is the only one. The host keeps
/// hover, keyboard selection, hit targets and titles per light.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Light {
    /// Claude's permission mode, one setting whatever its pill names: on in an
    /// expected mode ([`Mode::is_expected`]: bypass or auto), off otherwise.
    Mode,
}

impl Light {
    /// Every light, in the order the row draws them.
    pub const ALL: [Light; 1] = [Light::Mode];

    /// The light's word on its chip while its state is not known; the mode
    /// light's chip names its mode ([`Mode::word`]) once one is.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Light::Mode => "mode",
        }
    }

    /// The title a hover or the keyboard selection shows.
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Light::Mode => "Permission mode",
        }
    }
}

/// What a light shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LightState {
    /// The setting is on.
    On,
    /// The setting is off.
    Off,
    /// No screen has shown it yet, or the pill drawn is one this module does
    /// not know (a box over the composer keeps the last state shown).
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
    /// The mode a pill's anchor id (`mode.auto`, [`footer::pill_indicator`])
    /// names.
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

    /// Whether the owner expects this mode: bypass (auto-approve) or auto. A
    /// session in either is as expected and is never pressed away from.
    #[must_use]
    pub fn is_expected(self) -> bool {
        matches!(self, Mode::Bypass | Mode::Auto)
    }

    /// The mode's short word, as the light names it: `bypass`, `auto`,
    /// `accept edits`, `don't ask`, `plan`, `manual`.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Mode::Bypass => "bypass",
            Mode::Auto => "auto",
            Mode::AcceptEdits => "accept edits",
            Mode::DontAsk => "don't ask",
            Mode::Plan => "plan",
            Mode::Manual => "manual",
        }
    }

    /// The pill glyph Claude draws before the mode, from the vendor's table
    /// (`footer::PILLS`): `⏸` for plan and manual, `⏵⏵` for the rest.
    #[must_use]
    pub fn glyph(self) -> &'static str {
        footer::PILLS
            .iter()
            .find(|pill| Mode::of_indicator(pill.id) == Some(self))
            .map_or("", |pill| pill.glyph)
    }

    /// The mode's own ink where Claude gives it one (bypass's red, auto's
    /// warning amber); `None` draws it in the row's plain ink.
    #[must_use]
    pub fn hue(self) -> Option<[u8; 3]> {
        match self {
            Mode::Bypass => Some([0xE0, 0x5A, 0x5A]),
            Mode::Auto => Some([0xE8, 0xB0, 0x3C]),
            _ => None,
        }
    }
}

/// What one Claude Code screen says about the light's setting and the turn,
/// read off the engine where a toggle's next press is decided and for a live
/// upgrade's note; a frame folds in [`read_lights`] instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    /// The permission mode its pill names (every mode draws one, default
    /// included: `⏸ manual mode on`). `None` when no pill this build knows is
    /// under the composer.
    pub mode: Option<Mode>,
    /// A pill is drawn under the composer, known or not: a row opening with a
    /// pill glyph (`footer::opens_with_pill_glyph`), a cut one included.
    pub pill: bool,
    /// Claude is mid-turn (`aterm_phase::phase::busy_signal`, hard).
    pub busy: bool,
}

/// Read `rows` (one pane, top to bottom). `None` without a composer frame.
#[must_use]
pub fn read_screen(rows: &[String]) -> Option<Screen> {
    let (_, bottom) = aterm_phase::phase::composer_rules(rows)?;
    let Reading { mode, pill } = reading_under(rows, bottom);
    // A turn in flight, by the phase reader's rule (the spinner, `Still
    // working`, `esc to interrupt`, …); a background monitor alone (`soft`) is
    // not a turn.
    let busy = aterm_phase::phase::busy_signal(rows).is_some_and(|b| !b.soft);
    Some(Screen { mode, pill, busy })
}

/// What the lights show of `rows`: [`read`] of [`read_screen`], without the
/// turn. `None` without a composer frame. This is what a frame folds in; the
/// turn ([`Screen::busy`]) is read only off the engine, where a toggle's next
/// press is decided, never off a painted frame.
#[must_use]
pub fn read_lights(rows: &[String]) -> Option<Reading> {
    let (_, bottom) = aterm_phase::phase::composer_rules(rows)?;
    Some(reading_under(rows, bottom))
}

/// [`read_lights`] given the composer's bottom rule the caller has found (row
/// `bottom`): it reads only the rows under the rule, for the pill with its
/// `(shift+tab to cycle)` hint whole or cut (`footer::pill_row_under`).
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

    /// `light`'s state: the mode light is On in an expected mode
    /// ([`Mode::is_expected`]) and Off in any other.
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

    /// Fold a newer screen reading in. With no pill drawn (a box over the
    /// composer, a menu under it) the mode shown stays, so the light does not
    /// flicker under every dialog; a drawn pill this build cannot name reads
    /// Unknown.
    pub fn fold(&mut self, newer: &Reading) {
        if newer.mode.is_some() || newer.pill {
            self.mode = newer.mode;
        }
    }
}

/// Whether `light` shows something the owner does not expect: the one reason
/// a light is drawn at rest, and the one state a click acts in. A light this
/// screen does not show never deviates: nothing is drawn, or pressed, on a
/// guess.
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

/// shift+tab, as a terminal sends it: the mode light's one input. Safe with a
/// draft in the composer; Claude's mode cycle does not touch it.
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

    /// The drawn light keeps its mode under a box (no composer) and under a
    /// menu (no pill row), but a pill this build cannot name — renamed, or cut
    /// by a narrow pane — turns it Unknown rather than leaving the old mode up.
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
        // Control: a known pill moves it.
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
            // Cut mid-word: still the pill this build names.
            ("  \u{23F5}\u{23F5} auto mode on (shif", Mode::Auto),
        ];
        for (row, mode) in cases {
            let s = read_screen(&screen(&rule, row)).expect("a composer");
            assert_eq!(s.mode, Some(mode), "{row:?}");
            let r = read(Some(&s));
            assert_eq!(deviates(Light::Mode, &r), !mode.is_expected(), "{row:?}");
        }
    }

    /// A box over the composer: nothing is known, and nothing deviates — so
    /// nothing is drawn or pressed.
    #[test]
    fn no_composer_no_lights_and_no_drive() {
        assert_eq!(read_screen(&["$ ls".to_owned()]), None);
        assert_eq!(read(None), Reading::UNKNOWN);
        for light in Light::ALL {
            assert!(!deviates(light, &Reading::UNKNOWN));
        }
    }

    /// The mode light deviates in every mode but bypass and auto, so a click
    /// never presses away from either.
    #[test]
    fn each_toggle_is_claudes_own_input() {
        let at = |mode: Mode| Reading {
            mode: Some(mode),
            pill: true,
        };
        for mode in [Mode::Manual, Mode::AcceptEdits, Mode::Plan, Mode::DontAsk] {
            assert!(deviates(Light::Mode, &at(mode)), "{mode:?}");
        }
        for mode in [Mode::Bypass, Mode::Auto] {
            assert!(
                !deviates(Light::Mode, &at(mode)),
                "{mode:?}: never pressed away from"
            );
        }
    }

    /// EVERY RECORDED CLAUDE CODE SCREEN in `aterm-phase`'s fixtures (measured
    /// sessions and the 2.1.280 box captures): a screen with a composer gives
    /// a definite state for every light it shows, and one with a box up gives
    /// no screen at all — so a vendor change that moves the composer shows up
    /// here as a corpus failure, not as lights that quietly lie.
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
            // The mode light is definite exactly when a pill this build knows
            // is there.
            let pill = footer::mode_row(&rows).is_some();
            assert!(
                (got.state(Light::Mode) != LightState::Unknown) == pill,
                "{}: {got:?}",
                path.display()
            );
            if rows
                .iter()
                .any(|r| r.contains("\u{23F5}\u{23F5} auto mode on"))
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
    /// rule gives the same reading. Control: the corpus holds screens with a
    /// composer and screens without.
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
    /// as a mode. The directory is read, so new captures are judged as they
    /// land.
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

    /// THE 2.1.301 FOOTERS (measured 2026-10-15 by `aterm-lab footers`: the real
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

    /// THE CURRENT VENDOR, LIVE: every 2.1.283 capture (measured 2026-09-25,
    /// `aterm ctl text` of a private headless aterm, the owner's settings)
    /// reads as the state it was captured in — mode and turn in flight. A
    /// vendor build that draws any of these differently fails
    /// HERE, by name, before it can grey a light or press a wrong key.
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
