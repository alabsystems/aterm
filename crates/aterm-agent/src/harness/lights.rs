// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CLAUDE CODE LIGHTS — a compact row of lights at the end of aterm's
//! footer for the settings the owner always expects on (owner direction,
//! 2026-09-24): auto-approve, auto mode and fast mode. The effort tag Claude
//! Code writes into its own top rule already shows itself, so it has no light
//! of ours (owner, 2026-09-27: it "already has a UX element"); nor does
//! thinking, which the owner keeps on for good (owner, 2026-09-27: "that
//! should always be on") — a light that never changes says nothing.
//! Hover one to read its title; click it to toggle.
//!
//! Every light is read from what Claude Code ALREADY DRAWS and toggled
//! through what Claude Code ALREADY ACCEPTS — aterm keeps no second copy of a
//! vendor setting that could disagree with the vendor:
//!
//! | light | read from | toggled by |
//! |---|---|---|
//! | auto-approve | the mode pill: `⏵⏵ bypass permissions on` | shift+tab, Claude's own mode cycle |
//! | auto mode | the mode pill: `⏵⏵ auto mode on` | shift+tab |
//! | fast mode | the composer's top rule: `↯` or `fast mode` | `/fast on` · `/fast off` |
//!
//! Measured on 2.1.282: the top rule's text is the vendor's `topBorderText`,
//! the join of its effort tag, its fast-mode tag and an internal tag; the
//! pill pairing is the vendor's mode table (`footer::PILLS`).
//!
//! VERSION DRIFT. A light whose indicator is not on the screen reads
//! [`LightState::Unknown`], never a guess, and an unknown light cannot be
//! clicked. A toggle is a [`Drive`] the host performs and then READS BACK: the
//! light must change within [`SETTLE_MS`], else the host says so on the light
//! instead of pressing again. A vendor that renames a pill or moves a tag
//! therefore greys the light out; it can never make aterm type into a
//! composer it no longer understands.

use crate::harness::footer;

/// How long a toggle may take to show on Claude's screen before the host
/// calls it failed. A slash command runs in well under a second; this is the
/// ceiling, not the expectation.
pub const SETTLE_MS: u64 = 2500;

/// How many shift+tab presses a mode toggle may spend looking for its mode.
/// Claude's own cycle (2.1.283, the vendor's `hbt`) is default → accept
/// edits → plan → bypass (when the session allows it) → auto (when
/// available) → default: three to five modes; don't ask only ever leads back
/// to default. So a search is also stopped the moment the cycle comes back
/// round to a mode it has already shown (the host's lap check) — a target
/// this session's cycle does not hold is refused there, with the person's
/// mode restored, never pressed past. This bound is the backstop.
pub const MAX_MODE_PRESSES: u8 = 5;

/// One light.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Light {
    /// Claude's `bypass permissions` mode — every tool call approved.
    AutoApprove,
    /// Claude's `auto` mode — a classifier approves tool calls.
    AutoMode,
    /// Fast mode: faster output from the same model.
    Fast,
}

impl Light {
    /// Every light, in the order the row draws them.
    pub const ALL: [Light; 3] = [Light::AutoApprove, Light::AutoMode, Light::Fast];

    /// The title a hover or the keyboard selection shows.
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Light::AutoApprove => "Auto-approve (bypass permissions)",
            Light::AutoMode => "Auto mode",
            Light::Fast => "Fast mode",
        }
    }

    /// The light's colour when it is on — each its own, so the row reads at a
    /// glance without its titles. Claude Code's own inks where it has one
    /// (auto mode's warning amber, fast mode's orange).
    #[must_use]
    pub fn hue(self) -> [u8; 3] {
        match self {
            Light::AutoApprove => [0xE0, 0x5A, 0x5A],
            Light::AutoMode => [0xE8, 0xB0, 0x3C],
            Light::Fast => [0xFF, 0x8A, 0x3D],
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
    /// Claude's screen does not show it right now (a box covers the composer,
    /// or the vendor drew something this module does not know).
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
    fn of_indicator(indicator: &str) -> Option<Mode> {
        Some(match indicator {
            "bypass permissions" => Mode::Bypass,
            "auto mode" => Mode::Auto,
            "accept edits" => Mode::AcceptEdits,
            "don't ask" => Mode::DontAsk,
            "plan mode" => Mode::Plan,
            "manual mode" => Mode::Manual,
            _ => return None,
        })
    }
}

/// What one Claude Code screen says about the lights' settings — the one
/// parse the footer and the lights share per frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    /// The composer's top rule.
    pub top: usize,
    /// The composer's bottom rule.
    pub bottom: usize,
    /// The permission mode its pill names — every mode draws one on 2.1.283,
    /// default included (`⏸ manual mode on`, measured 2026-09-25). `None`
    /// when no pill this build knows is under the composer, and then the mode
    /// lights read Unknown rather than a guessed mode.
    pub mode: Option<Mode>,
    /// Claude is mid-turn (`aterm_phase::phase::busy_signal`, hard).
    pub busy: bool,
    /// The words Claude Code wrote into the top rule (its effort tag, `↯`, …).
    pub tags: Vec<String>,
}

/// Whether the mode pill `pill` (its own text, `⏵⏵ auto mode on`) names a
/// mode one of the lights shows. With the lights on the row the footer drops
/// such a pill instead of showing the mode twice; a mode no light shows
/// (plan, accept edits, don't ask, manual) keeps its pill.
#[must_use]
pub fn pill_has_light(pill: &str) -> bool {
    footer::pill_indicator(&format!("  {pill}"))
        .and_then(Mode::of_indicator)
        .is_some_and(|mode| matches!(mode, Mode::Bypass | Mode::Auto))
}

/// Read `rows` (one pane, top to bottom). `None` without a composer frame.
#[must_use]
pub fn read_screen(rows: &[String]) -> Option<Screen> {
    let (top, bottom) = aterm_phase::phase::composer_rules(rows)?;
    let mode = footer::mode_row(rows)
        .and_then(|r| footer::pill_indicator(&rows[r]))
        .and_then(Mode::of_indicator);
    // A turn in flight, by the phase reader's own rule (the spinner, `Still
    // working`, `esc to interrupt` under the composer, …) — a background
    // monitor alone (`soft`) is not a turn.
    let busy = aterm_phase::phase::busy_signal(rows).is_some_and(|b| !b.soft);
    Some(Screen {
        top,
        bottom,
        mode,
        busy,
        tags: rule_tags(&rows[top]),
    })
}

/// Whether Claude's composer holds EXACTLY `cmd` with the cursor right behind
/// it: the composer frame is drawn (no box over it), `cmd` is alone on the
/// draft's first line, no other draft line has text, and the terminal cursor
/// sits at the end of the caret row. The one shape where an Enter submits
/// `cmd` and nothing else — and where one ctrl+u (kill to line start) takes
/// back exactly what was typed. A cursor at the caret's start (a placeholder
/// suggestion, a homed caret), a character past the text or a second line
/// all fail it: the composer holds something aterm did not type.
#[must_use]
pub fn composer_holds(rows: &[String], cursor: (usize, usize), cmd: &str) -> bool {
    if aterm_phase::phase::composer_rules(rows).is_none() {
        return false;
    }
    let Some((caret, lines)) = aterm_phase::phase::composer_draft(rows) else {
        return false;
    };
    lines.first().is_some_and(|line| line == cmd)
        && lines.iter().skip(1).all(String::is_empty)
        && cursor == (caret, rows[caret].trim_end().chars().count())
}

/// The words inside a composer rule: `──── <tag> ↯ ─` → `<tag>`, `↯`.
/// A `·` between tags is a separator, not a word.
#[must_use]
pub fn rule_tags(rule: &str) -> Vec<String> {
    rule.split(['\u{2500}', '\u{00B7}'])
        .flat_map(str::split_whitespace)
        .map(str::to_owned)
        .collect()
}

/// Every light's state on `screen`. Without a screen every light is
/// [`LightState::Unknown`] — the host keeps showing the last known states
/// while a box covers the composer.
#[must_use]
pub fn states(screen: Option<&Screen>) -> [LightState; 3] {
    let Some(s) = screen else {
        return [LightState::Unknown; 3];
    };
    let on = |b: bool| if b { LightState::On } else { LightState::Off };
    let has = |word: &str| s.tags.iter().any(|t| t == word);
    Light::ALL.map(|light| match light {
        Light::AutoApprove => s
            .mode
            .map_or(LightState::Unknown, |m| on(m == Mode::Bypass)),
        Light::AutoMode => s.mode.map_or(LightState::Unknown, |m| on(m == Mode::Auto)),
        // `↯`, or the words `fast mode` (the vendor's narrow-terminal spelling,
        // `fast mode (cooling down)` included).
        Light::Fast => {
            on(has("\u{21AF}") || s.tags.windows(2).any(|w| w[0] == "fast" && w[1] == "mode"))
        }
    })
}

/// How to toggle a light: Claude Code's own inputs, and what must hold before
/// the host may send them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Drive {
    /// Press shift+tab, read the pill back, and press again until the mode is
    /// `until` — or, for `None`, until it is any mode but the one it was —
    /// giving up once the cycle laps (see [`MAX_MODE_PRESSES`]). Safe with a
    /// draft in the composer: the mode cycle does not touch it.
    CycleMode {
        /// The mode to stop at; `None` = leave the current one.
        until: Option<Mode>,
    },
    /// Submit this slash command. ONLY into an EMPTY composer
    /// (`upgrade::composer_is_empty`): typed over a draft it would be sent
    /// with the draft.
    Command(&'static str),
}

/// shift+tab, as a terminal sends it.
pub const SHIFT_TAB: &[u8] = b"\x1b[Z";

/// How to flip `light` from `state`. `None` for an unknown light — nothing is
/// typed into a screen this module cannot read back.
#[must_use]
pub fn drive(light: Light, state: LightState) -> Option<Drive> {
    let on = match state {
        LightState::On => true,
        LightState::Off => false,
        LightState::Unknown => return None,
    };
    Some(match (light, on) {
        (Light::AutoApprove, false) => Drive::CycleMode {
            until: Some(Mode::Bypass),
        },
        (Light::AutoMode, false) => Drive::CycleMode {
            until: Some(Mode::Auto),
        },
        (Light::AutoApprove | Light::AutoMode, true) => Drive::CycleMode { until: None },
        (Light::Fast, false) => Drive::Command("/fast on"),
        (Light::Fast, true) => Drive::Command("/fast off"),
    })
}

/// Whether a mode cycle has arrived: at `until`, or — for `None` — anywhere
/// but where it started.
#[must_use]
pub fn cycle_done(until: Option<Mode>, started: Mode, now: Mode) -> bool {
    match until {
        Some(target) => now == target,
        None => now != started,
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

    /// The owner's own screen, measured 2.1.282: bypass, with an effort tag
    /// on the rule (`workspace` stands in for the vendor's word) that no
    /// light reads.
    #[test]
    fn the_lights_read_claudes_own_indicators() {
        let rule = format!("{} workspace \u{2500}", "\u{2500}".repeat(50));
        let rows = screen(&rule, BYPASS);
        let s = read_screen(&rows).expect("a composer");
        assert_eq!(s.mode, Some(Mode::Bypass));
        assert!(!s.busy);
        assert_eq!(
            states(Some(&s)),
            [LightState::On, LightState::Off, LightState::Off]
        );
        let rule = format!("{} workspace \u{21AF} \u{2500}", "\u{2500}".repeat(50));
        let auto = "  \u{23F5}\u{23F5} auto mode on (shift+tab to cycle)";
        let s = read_screen(&screen(&rule, auto)).unwrap();
        assert_eq!(
            states(Some(&s)),
            [LightState::Off, LightState::On, LightState::On]
        );
    }

    #[test]
    fn fast_mode_spelled_out_is_fast_mode() {
        let rule = format!(
            "{} fast mode (cooling down) \u{2500}",
            "\u{2500}".repeat(40)
        );
        let s = read_screen(&screen(&rule, BYPASS)).unwrap();
        assert_eq!(states(Some(&s))[2], LightState::On);
    }

    /// Claude's default mode draws its own pill (2.1.283, measured
    /// 2026-09-25): manual, and neither permission light is on.
    #[test]
    fn the_manual_pill_is_the_default_mode() {
        let manual =
            "  \u{23F8} manual mode on \u{00B7} ? for shortcuts \u{00B7} \u{2190} for agents";
        let s = read_screen(&screen(&"\u{2500}".repeat(60), manual)).unwrap();
        assert_eq!(s.mode, Some(Mode::Manual));
        assert_eq!(states(Some(&s))[..2], [LightState::Off, LightState::Off]);
    }

    /// A row under the composer that names no pill this build knows is NOT
    /// read as some mode: the mode lights are Unknown, so nothing is pressed
    /// on the strength of a guess (a renamed pill, a wrapped hint).
    #[test]
    fn a_pill_this_build_does_not_know_is_unknown() {
        for row in ["  ? for shortcuts", "  \u{23F5}\u{23F5} turbo mode on"] {
            let s = read_screen(&screen(&"\u{2500}".repeat(60), row)).unwrap();
            assert_eq!(s.mode, None, "{row:?}");
            assert_eq!(
                states(Some(&s))[..2],
                [LightState::Unknown, LightState::Unknown],
                "{row:?}"
            );
        }
    }

    /// Claude mid-turn: `esc to interrupt` under the composer.
    #[test]
    fn a_turn_in_flight_reads_busy() {
        let busy = "  \u{23F5}\u{23F5} auto mode on \u{00B7} esc to interrupt";
        assert!(
            read_screen(&screen(&"\u{2500}".repeat(60), busy))
                .unwrap()
                .busy
        );
    }

    /// A box over the composer: nothing is known, and nothing may be typed.
    #[test]
    fn no_composer_no_lights_and_no_drive() {
        assert_eq!(read_screen(&["$ ls".to_owned()]), None);
        assert_eq!(states(None), [LightState::Unknown; 3]);
        for light in Light::ALL {
            assert_eq!(drive(light, LightState::Unknown), None);
        }
    }

    #[test]
    fn each_toggle_is_claudes_own_input() {
        assert_eq!(
            drive(Light::AutoMode, LightState::Off),
            Some(Drive::CycleMode {
                until: Some(Mode::Auto)
            })
        );
        assert_eq!(
            drive(Light::AutoApprove, LightState::On),
            Some(Drive::CycleMode { until: None })
        );
        assert_eq!(
            drive(Light::Fast, LightState::On),
            Some(Drive::Command("/fast off"))
        );
        assert!(cycle_done(Some(Mode::Auto), Mode::Bypass, Mode::Auto));
        assert!(!cycle_done(Some(Mode::Auto), Mode::Bypass, Mode::Plan));
        assert!(cycle_done(None, Mode::Bypass, Mode::Plan));
        assert!(!cycle_done(None, Mode::Bypass, Mode::Bypass));
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
        let mut read = 0;
        for entry in std::fs::read_dir(&dir).expect("the phase fixtures") {
            let path = entry.unwrap().path();
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let rows: Vec<String> = text.lines().map(str::to_owned).collect();
            let Some(s) = read_screen(&rows) else {
                continue;
            };
            let got = states(Some(&s));
            // Fast mode's rule tag is always readable under a composer; the mode
            // lights are definite exactly when a pill this build knows is there.
            let pill = footer::mode_row(&rows).is_some();
            assert!(
                got[2] != LightState::Unknown
                    && got[..2]
                        .iter()
                        .all(|st| (*st != LightState::Unknown) == pill),
                "{}: {got:?}",
                path.display()
            );
            if rows
                .iter()
                .any(|r| r.contains("\u{23F5}\u{23F5} auto mode on"))
            {
                assert_eq!(s.mode, Some(Mode::Auto), "{}", path.display());
            }
            read += 1;
        }
        assert!(read >= 10, "the corpus must exercise the reader: {read}");
    }

    /// THE CURRENT VENDOR, LIVE: every 2.1.283 capture (measured 2026-09-25,
    /// `aterm ctl text` of a private headless aterm, the owner's settings)
    /// reads as the state it was captured in — mode and turn in flight, with
    /// the effort tag on the rule (`workspace` stands in for the vendor's
    /// word) read by no light. A vendor build that draws any of these differently fails
    /// HERE, by name, before it can grey a light or press a wrong key.
    /// THE DRIFT CANARY. Every recorded FOOTER screen of every Claude Code
    /// build in the corpus (`claude-<version>-footer-*.txt` in aterm-phase's
    /// fixtures, recorded by `tools/capture-claude-screens.sh`) has a composer
    /// this build's reader sees, a mode row the footer plans, and a pill the
    /// footer AND the lights read as a mode. So a new Claude Code, captured
    /// before sessions run on it, that draws its mode row differently fails
    /// here — by file and build — where the running window could only step
    /// aside and name it in aterm.log. The directory is read, not listed: a new
    /// build's captures are judged the moment they land.
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
                .unwrap_or_else(|| panic!("{name}: no mode row the footer plans"));
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
            assert!(
                s.tags.iter().any(|t| t == "workspace"),
                "{name}: the effort tag is read off the rule"
            );
            assert_eq!(
                states(Some(&s))[2],
                LightState::Off,
                "{name}: an effort tag is not fast mode"
            );
            assert!(
                footer::mode_row(&rows).is_some_and(|r| footer::plan_row(&rows[r]).is_some()),
                "{name}: the footer rewrites its mode row"
            );
        }
    }

    /// Only the exact shape: the command alone, the cursor right behind it.
    #[test]
    fn the_composer_holds_the_command_only_exactly() {
        let rule = "\u{2500}".repeat(40);
        let screen = |caret: &str, second: &str| -> Vec<String> {
            let mut rows = vec![rule.clone(), format!("\u{276F} {caret}")];
            if !second.is_empty() {
                rows.push(format!("  {second}"));
            }
            rows.push(rule.clone());
            rows.push("  \u{23F5}\u{23F5} auto mode on".into());
            rows
        };
        let cmd = "/fast on";
        assert!(composer_holds(&screen(cmd, ""), (1, 10), cmd));
        for (rows, cursor, why) in [
            (
                screen("/fast onh", ""),
                (1, 11),
                "a character typed after it",
            ),
            (screen(cmd, "hello"), (1, 10), "a second line"),
            (screen(cmd, ""), (1, 2), "a placeholder or a homed caret"),
            (screen(cmd, ""), (1, 11), "a space typed after it"),
            (screen("/fast", ""), (1, 7), "only part of it"),
        ] {
            assert!(!composer_holds(&rows, cursor, cmd), "{why}");
        }
        assert!(
            !composer_holds(&[rule.clone(), format!("\u{276F} {cmd}")], (1, 10), cmd),
            "no frame: a box over the composer"
        );
    }

    #[test]
    fn only_a_mode_with_a_light_gives_its_pill_up() {
        assert!(pill_has_light("\u{23F5}\u{23F5} auto mode on"));
        assert!(pill_has_light("\u{23F5}\u{23F5} bypass permissions on"));
        assert!(!pill_has_light("\u{23F8} plan mode on"));
        assert!(!pill_has_light("\u{23F5}\u{23F5} accept edits on"));
        assert!(!pill_has_light("\u{23F8} manual mode on"));
        assert!(!pill_has_light("esc to interrupt"));
    }

    #[test]
    fn a_rule_holds_words_not_dashes() {
        assert_eq!(
            rule_tags("\u{2500}\u{2500}\u{2500} workspace \u{00B7} \u{21AF} \u{2500}"),
            ["workspace", "\u{21AF}"]
        );
        assert!(rule_tags(&"\u{2500}".repeat(20)).is_empty());
    }
}
