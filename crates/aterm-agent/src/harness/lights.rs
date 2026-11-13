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
    /// Claude's permission mode, one bit whatever its pill names: yolo on in bypass, off in every
    /// other mode ([`footer::YOLO`], one word either way).
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

    /// The mode as Claude Code's settings and its `--permission-mode` flag spell it
    /// (`bypassPermissions`; manual is `default`), [`Self::of_setting`]'s inverse.
    #[must_use]
    pub const fn setting(self) -> &'static str {
        match self {
            Mode::Bypass => "bypassPermissions",
            Mode::Auto => "auto",
            Mode::AcceptEdits => "acceptEdits",
            Mode::DontAsk => "dontAsk",
            Mode::Plan => "plan",
            Mode::Manual => "default",
        }
    }

    /// The mode a settings document's `permissions.defaultMode` or a `--permission-mode` value
    /// names: [`Self::setting`]'s words, and `manual`, which Claude Code reads as `default`.
    #[must_use]
    pub fn of_setting(word: &str) -> Option<Mode> {
        Some(match word {
            "bypassPermissions" => Mode::Bypass,
            "auto" => Mode::Auto,
            "acceptEdits" => Mode::AcceptEdits,
            "dontAsk" => Mode::DontAsk,
            "plan" => Mode::Plan,
            "default" | "manual" => Mode::Manual,
            _ => return None,
        })
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
#[path = "lights_tests.rs"]
mod tests;
