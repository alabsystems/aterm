// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CLAUDE CODE LIGHTS — a light beside aterm's footer for the setting
//! the owner always expects on: the permission mode. The effort tag Claude
//! Code writes into its own top rule already shows itself, so it has no light
//! of ours; nor does thinking, which the owner keeps on for good — a light
//! that never changes says nothing. Nor does fast mode: Claude Code draws its
//! own `↯` in the composer's top rule while fast mode is on, so aterm draws
//! no chip and types no `/fast` (the footer still reads a `/fast` result that
//! moved the model, for its model fact).
//!
//! DEVIATIONS ONLY. The light is drawn only while its setting DIFFERS from
//! what the owner expects ([`deviates`]) — or while a toggle of it is in
//! flight, or its answer is on show. At rest, everything as expected, no
//! light is drawn beside the footer's facts in the composer's bottom rule,
//! and Claude's own rows are whole. Hover a drawn light to read its title;
//! click it to toggle.
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
//! One permission-mode light: auto-approve (`bypass permissions`) and auto mode
//! are two positions of Claude Code's one permission mode, and the owner
//! expects either ([`Mode::is_expected`]). The light is on in both, and its one
//! action, from any other mode, runs forward through Claude's own cycle to the
//! nearest of them, never away from one.
//!
//! The light is read from what Claude Code ALREADY DRAWS and toggled through
//! what Claude Code ALREADY ACCEPTS — aterm keeps no second copy of a vendor
//! setting that could disagree with the vendor: read from the mode pill
//! (`⏵⏵ bypass permissions on`, `⏸ plan mode on`, …; the pairing is the
//! vendor's mode table, `footer::PILLS`), toggled by shift+tab, Claude's own
//! mode cycle, forward to bypass or auto (from don't ask too) — aterm's own
//! next presses only while Claude is idle.
//!
//! VERSION DRIFT. A light reads [`LightState::Unknown`] until a screen shows
//! its pill, and whenever the pill drawn is one this build cannot name
//! (renamed, or cut by a narrow pane); a box over the composer or a menu
//! under it keeps the last mode read ([`Reading::fold`]). An unknown light
//! cannot be clicked. A toggle is a press the host makes and then READS BACK: each
//! press must be answered within its own window ([`SETTLE_MS`]), and a next
//! press goes only on an answer READ — once the pill shows Claude took the
//! last press, never on a timer — else the host says on the light where it
//! stopped. A vendor that renames a pill therefore greys the light out; it
//! can never make aterm press keys into a screen it no longer understands.

use crate::harness::footer;

/// How long ONE shift+tab of a mode return may take to show on Claude's
/// screen before the host calls it failed (a mode return refreshes it on
/// every press). The ceiling, not the expectation — a pill redraw lands
/// within a frame or two.
pub const SETTLE_MS: u64 = 2500;

/// How many shift+tab presses a mode toggle may spend on its way back to an
/// expected mode. Claude's own cycle (2.1.283, the vendor's `hbt`) is
/// default → accept edits → plan → bypass (when the session allows it) →
/// auto (when available) → default: three to five modes; don't ask only
/// ever leads on to default. The nearest expected mode is at most three
/// presses away from manual, four from don't ask. A search
/// is also stopped the moment the cycle comes back round to a mode it has
/// already shown (the host's lap check) — a cycle that holds neither bypass
/// nor auto is refused there, naming the mode it stopped in, never pressed
/// past. This bound is the backstop.
pub const MAX_MODE_PRESSES: u8 = 5;

/// One light of the row — today the one, the permission mode. The host's
/// hover, keyboard selection, hit targets and titles are kept per light.
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
    /// light's chip names its MODE ([`Mode::word`]) once one is.
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

    /// The pill glyph Claude draws before the mode, read off the vendor's
    /// table (`footer::PILLS`): `⏸` for plan and manual, `⏵⏵` for the rest.
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

/// What one Claude Code screen says about the light's setting and the turn
/// — read off the engine where a toggle's next press is decided, and for a
/// live upgrade's note (what a frame folds in is [`read_lights`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    /// The permission mode its pill names — every mode draws one on 2.1.283,
    /// default included (`⏸ manual mode on`, measured 2026-09-25). `None`
    /// when no pill this build knows is under the composer; the light then
    /// reads Unknown only if a pill is drawn ([`Screen::pill`]), and otherwise
    /// keeps the mode last shown ([`Reading::fold`]).
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

/// What the lights SHOW of `rows` — [`read_screen`]'s reading ([`read`] of
/// it) without the turn: the mode its pill names and whether a pill is
/// drawn. `None` without a composer frame. What a frame folds in: the turn
/// is read only where a toggle's next press is decided, off the engine
/// ([`Screen::busy`]), never off a painted frame.
#[must_use]
pub fn read_lights(rows: &[String]) -> Option<Reading> {
    let (_, bottom) = aterm_phase::phase::composer_rules(rows)?;
    Some(reading_under(rows, bottom))
}

/// [`read_lights`] under the composer bottom rule the caller has found (row
/// `bottom`): the pill this build sees, its `(shift+tab to cycle)` hint whole
/// or cut (`footer::pill_row_under`) — a reading of the rows under the rule
/// alone.
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

/// Whether `light` shows something the owner does NOT expect — the one
/// reason a light is drawn at rest, and the one state a click acts in: the
/// mode light deviates in any mode but bypass and auto, and its click
/// presses forward from there. A light this screen does not show never
/// deviates: nothing is drawn, or pressed, on a guess.
#[must_use]
pub fn deviates(light: Light, reading: &Reading) -> bool {
    reading.state(light) == LightState::Off
}

/// What `screen` says of each light. Without a screen nothing is known
/// ([`Reading::UNKNOWN`]); the host keeps the last known states while a box
/// covers the composer.
#[must_use]
pub fn read(screen: Option<&Screen>) -> Reading {
    screen.map_or(Reading::UNKNOWN, |s| Reading {
        mode: s.mode,
        pill: s.pill,
    })
}

/// shift+tab, as a terminal sends it: the mode light's one input. Safe with
/// a draft in the composer — Claude's mode cycle does not touch it.
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

    /// A narrow pane cuts the `(shift+tab to cycle)` hint, between words or
    /// mid-word: the pill still names the mode, and a plan-mode pill is still
    /// one the light cycles away from.
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

    /// The mode light's one action is FORWARD to bypass or auto, and only
    /// from another mode: in either expected mode it does not deviate and
    /// there is nothing to press, so a click never leaves auto mode.
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

    /// WHAT A FRAME FOLDS IN IS WHAT THE SCREEN SAYS: on every recorded
    /// screen in `aterm-phase`'s fixtures, [`read_lights`] — the lights'
    /// reading with no turn read — is [`read`] of [`read_screen`], and
    /// [`reading_under`] the composer's bottom rule is that same reading.
    /// CONTROL: the corpus holds both, screens with a composer and screens
    /// with none.
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

    /// THE DRIFT CANARY. Every recorded FOOTER screen of every Claude Code
    /// build in the corpus (`claude-<version>-footer-*.txt` in aterm-phase's
    /// fixtures, recorded by `aterm-lab footers`) has a composer
    /// this reader sees, a mode row under it, and a pill the footer and the
    /// lights read as a mode. A new build that draws its mode row differently
    /// fails here by file and build. The directory is read, not listed, so new
    /// captures are judged as they land.
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
