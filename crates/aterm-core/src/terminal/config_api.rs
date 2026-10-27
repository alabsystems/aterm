// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Configuration hot-reload (`Terminal::apply_config` and `Terminal::rebase_colors`).

use super::Terminal;

#[inline]
const fn apply_negotiated_mode(live: &mut bool, seed: &mut bool, configured: bool) -> bool {
    if *seed == configured {
        return false;
    }
    *seed = configured;
    let moved = *live != configured;
    *live = configured;
    moved
}

struct LiveColors {
    cursor: Option<aterm_types::Rgb>,
    rest: (
        aterm_types::Rgb,
        aterm_types::Rgb,
        Option<aterm_types::Rgb>,
        Option<aterm_types::Rgb>,
        aterm_types::ColorPalette,
    ),
}

impl LiveColors {
    #[inline]
    fn of(c: &super::grouped_state::ColorState) -> Self {
        Self {
            cursor: c.cursor_color,
            rest: (
                c.default_foreground,
                c.default_background,
                c.selection_background,
                c.selection_foreground,
                c.palette().clone(),
            ),
        }
    }
}

/// Host colour baseline that OSC 104/110-119 and RIS reset to and DSR 996 reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColorBaseline {
    /// The configured default foreground (OSC 110 resets to it).
    pub foreground: aterm_types::Rgb,
    /// The configured default background (OSC 111 resets to it).
    pub background: aterm_types::Rgb,
    /// The configured cursor colour; `None` follows the foreground.
    pub cursor: Option<aterm_types::Rgb>,
    /// The configured selection background; `None` leaves it to the renderer.
    pub selection_background: Option<aterm_types::Rgb>,
    /// The configured selection foreground; `None` leaves it to the renderer.
    pub selection_foreground: Option<aterm_types::Rgb>,
    /// The configured palette; `None` is the xterm table.
    pub palette: Option<aterm_types::ColorPalette>,
    /// The host's declared appearance of `background`; `None` keeps the
    /// terminal's current one (as `TerminalConfig::color_scheme` does).
    pub color_scheme: Option<aterm_types::Appearance>,
}

impl ColorBaseline {
    /// Extract the colour baseline from a [`TerminalConfig`](crate::config::TerminalConfig).
    #[inline]
    #[must_use]
    pub fn of_config(c: &crate::config::TerminalConfig) -> Self {
        Self {
            foreground: c.default_foreground,
            background: c.default_background,
            cursor: c.cursor_color,
            selection_background: c.selection_background,
            selection_foreground: c.selection_foreground,
            palette: c.custom_palette.clone(),
            color_scheme: c.color_scheme,
        }
    }
}

/// Live colour changes produced by [`Terminal::rebase_colors`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct ColorRebase {
    /// The live cursor colour moved.
    pub cursor_changed: bool,
    /// Any other live colour (fg, bg, selection, palette) moved.
    pub colors_changed: bool,
    /// The reported scheme ([`Terminal::color_scheme`]) moved; a
    /// `CSI ? 997 ; Ps n` push is queued when DEC 2031 is set.
    pub scheme_changed: bool,
}

impl Terminal {
    /// Current host colour baseline.
    #[inline]
    #[must_use]
    pub fn color_baseline(&self) -> ColorBaseline {
        let c = &self.color;
        ColorBaseline {
            foreground: c.configured_foreground,
            background: c.configured_background,
            cursor: c.configured_cursor,
            selection_background: c.configured_selection_background,
            selection_foreground: c.configured_selection_foreground,
            palette: c.configured_palette.clone(),
            color_scheme: Some(c.configured_color_scheme),
        }
    }

    /// Update the theme baseline while preserving active OSC colour overrides on top.
    #[inline]
    pub fn rebase_colors(&mut self, baseline: &ColorBaseline) -> ColorRebase {
        let rebase = self.rebase_colors_inner(baseline, self.modes.allow_palette_reconfigure);
        if rebase.cursor_changed || rebase.colors_changed {
            self.grid.damage_mut().mark_full();
        }
        rebase
    }

    /// Draw in `config`'s colours and colour policy and change nothing else, so every mode the program set (cursor shape and visibility among them) stays.
    pub fn apply_config_colors(&mut self, config: &crate::config::TerminalConfig) {
        let rebase = self.rebase_colors_inner(
            &ColorBaseline::of_config(config),
            config.allow_palette_reconfigure,
        );
        let policy = self.apply_style_policy(config);
        if rebase.cursor_changed || rebase.colors_changed || policy {
            self.grid.damage_mut().mark_full();
        }
    }

    /// Take `config`'s bold-is-bright, faint opacity and indexed contrast floor: whether any moved.
    fn apply_style_policy(&mut self, config: &crate::config::TerminalConfig) -> bool {
        let new_faint = if config.faint_opacity.is_finite() {
            config.faint_opacity.clamp(0.0, 1.0)
        } else {
            aterm_types::DIM_FACTOR
        };
        let new_floor = if config.indexed_minimum_contrast.is_finite() {
            config.indexed_minimum_contrast.clamp(1.0, 21.0)
        } else {
            1.0
        };
        let moved = self.color.bold_is_bright != config.bold_is_bright
            || (self.color.faint_opacity - new_faint).abs() > f32::EPSILON
            || (self.color.indexed_minimum_contrast - new_floor).abs() > f32::EPSILON;
        if moved {
            self.color.bold_is_bright = config.bold_is_bright;
            self.color.faint_opacity = new_faint;
            self.color.indexed_minimum_contrast = new_floor;
        }
        moved
    }

    fn rebase_colors_inner(&mut self, b: &ColorBaseline, palette_gate: bool) -> ColorRebase {
        let before = LiveColors::of(&self.color);
        let overrides = self.color.capture_overrides();
        self.color.configured_cursor = b.cursor;
        self.color.configured_foreground = b.foreground;
        self.color.configured_background = b.background;
        if let Some(scheme) = b.color_scheme {
            self.color.configured_color_scheme = scheme;
        }
        self.color.configured_selection_background = b.selection_background;
        self.color.configured_selection_foreground = b.selection_foreground;
        self.color.configured_palette.clone_from(&b.palette);
        self.color.restore_baseline(true);
        self.color.apply_overrides(&overrides, palette_gate);
        self.color.frame_cursor_authoritative = true;
        self.color.frame_foreground_authoritative = true;
        self.color.frame_background_authoritative = true;
        let after = LiveColors::of(&self.color);
        let scheme_changed = self.reconcile_color_scheme();
        ColorRebase {
            cursor_changed: before.cursor != after.cursor,
            colors_changed: before.rest != after.rest,
            scheme_changed,
        }
    }

    /// Apply configuration changes to the terminal at runtime and return the
    /// aspects that changed.
    ///
    /// # Example
    ///
    /// ```
    /// use aterm_core::config::{ConfigChange, TerminalConfig};
    /// use aterm_core::terminal::{CursorStyle, Terminal};
    ///
    /// let mut term = Terminal::new(24, 80);
    ///
    /// // Create new configuration: a STEADY bar, where the terminal's power-on
    /// // cursor is a blinking block — both the shape and the blink move.
    /// let mut config = TerminalConfig::default();
    /// config.cursor_style = CursorStyle::SteadyBar;
    ///
    /// // Apply and check what changed
    /// let changes = term.apply_config(&config);
    /// assert!(changes.contains(&ConfigChange::CursorStyle));
    /// assert!(changes.contains(&ConfigChange::CursorBlink));
    /// ```
    #[allow(
        clippy::too_many_lines,
        reason = "flat field-by-field config application"
    )]
    pub fn apply_config(
        &mut self,
        config: &crate::config::TerminalConfig,
    ) -> Vec<crate::config::ConfigChange> {
        use crate::config::ConfigChange;

        let mut changes = Vec::new();
        let mut visual_repaint = false;

        self.default_cursor_style = config.cursor_style;
        if self.modes.cursor_style != config.cursor_style {
            self.modes.cursor_style = config.cursor_style;
            changes.push(ConfigChange::CursorStyle);
        }

        if self.modes.cursor_blink != self.modes.cursor_style.blinks() {
            self.modes.cursor_blink = self.modes.cursor_style.blinks();
            changes.push(ConfigChange::CursorBlink);
        }

        let rebase = self.rebase_colors_inner(
            &ColorBaseline::of_config(config),
            config.allow_palette_reconfigure,
        );
        if rebase.scheme_changed {
            changes.push(ConfigChange::ColorScheme);
        }

        if rebase.cursor_changed {
            changes.push(ConfigChange::CursorColor);
            visual_repaint = true;
        }

        if self.modes.cursor_visible != config.cursor_visible {
            self.modes.cursor_visible = config.cursor_visible;
            changes.push(ConfigChange::CursorVisible);
        }

        if rebase.colors_changed {
            changes.push(ConfigChange::Colors);
            visual_repaint = true;
        }

        if apply_negotiated_mode(
            &mut self.modes.auto_wrap,
            &mut self.configured_modes.auto_wrap,
            config.auto_wrap,
        ) {
            changes.push(ConfigChange::AutoWrap);
        }
        if apply_negotiated_mode(
            &mut self.modes.focus_reporting,
            &mut self.configured_modes.focus_reporting,
            config.focus_reporting,
        ) {
            changes.push(ConfigChange::FocusReporting);
        }
        if apply_negotiated_mode(
            &mut self.modes.bracketed_paste,
            &mut self.configured_modes.bracketed_paste,
            config.bracketed_paste,
        ) {
            changes.push(ConfigChange::BracketedPaste);
        }

        if self.modes.allow_osc52_query != config.allow_osc52_query {
            self.modes.allow_osc52_query = config.allow_osc52_query;
            if config.allow_osc52_query {
                self.clipboard_auth.authorize_query();
            } else {
                self.clipboard_auth.revoke_query();
            }
            changes.push(ConfigChange::Osc52ClipboardQuery);
        }

        if self.modes.allow_window_ops != config.allow_window_ops {
            self.modes.allow_window_ops = config.allow_window_ops;
            changes.push(ConfigChange::WindowOps);
        }

        if self.modes.allow_notifications != config.allow_notifications {
            self.modes.allow_notifications = config.allow_notifications;
            changes.push(ConfigChange::Notifications);
        }

        if self.modes.allow_palette_reconfigure != config.allow_palette_reconfigure {
            self.set_allow_palette_reconfigure(config.allow_palette_reconfigure);
            changes.push(ConfigChange::PaletteReconfigure);
        }

        if self.modes.ambiguous_width_double != config.ambiguous_width_double {
            self.modes.ambiguous_width_double = config.ambiguous_width_double;
            changes.push(ConfigChange::AmbiguousWidth);
        }

        if self.apply_style_policy(config) {
            changes.push(ConfigChange::StylePolicy);
            visual_repaint = true;
        }

        let current_budget = self
            .main_grid()
            .scrollback_memory_budget()
            .unwrap_or(config.memory_budget);
        if current_budget != config.memory_budget {
            if let Err(e) = self.set_memory_budget(config.memory_budget) {
                aterm_log::warn!(
                    "apply_config: memory budget enforcement failed ({e}), \
                     budget stored but not fully enforced"
                );
            }
            changes.push(ConfigChange::MemoryBudget);
        }

        if self.bidi_state.config != config.bidi {
            self.bidi_state.config = config.bidi.clone();
            self.modes.bidi_mode = config.bidi.mode;
            self.modes.bidi_direction = config.bidi.direction;
            self.invalidate_bidi_all();
            changes.push(ConfigChange::BiDi);
        }

        let current_limit = self.scrollback_line_limit();
        let new_limit = config.scrollback_limit;
        if current_limit != new_limit {
            self.set_scrollback_line_limit(new_limit);
            changes.push(ConfigChange::ScrollbackLimit);
        }

        let new_timeout = std::time::Duration::from_millis(config.sync_timeout_ms);
        if self.sync_timeout_duration != new_timeout {
            self.sync_timeout_duration = new_timeout;
            changes.push(ConfigChange::SyncTimeout);
        }

        self.refresh_mode_mirror();

        if visual_repaint {
            self.grid.damage_mut().mark_full();
        }

        changes
    }

    /// [`Self::apply_config`] for an engine a checkpoint restored: it takes `config`'s colour baseline, default cursor style, mode seeds, budgets and policies, keeps every mode and the history limit the restore installed, and leaves any colour-scheme report the rebase queued for the caller to drain.
    pub fn apply_config_keeping_modes(&mut self, config: &crate::config::TerminalConfig) {
        let modes = self.modes;
        let limit = self.scrollback_line_limit();
        let _ = self.apply_config(config);
        self.modes = modes;
        if self.scrollback_line_limit() != limit {
            self.set_scrollback_line_limit(limit);
        }
        if modes.allow_osc52_query {
            self.clipboard_auth.authorize_query();
        } else {
            self.clipboard_auth.revoke_query();
        }
        self.refresh_mode_mirror();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_mode_seed_holds_exactly_what_a_default_host_config_carries() {
        let host = crate::config::TerminalConfig::default();
        let seed = super::super::state::ConfiguredModes::default();
        assert_eq!(seed.auto_wrap, host.auto_wrap, "auto_wrap (DECAWM)");
        assert_eq!(
            seed.bracketed_paste, host.bracketed_paste,
            "bracketed_paste (DEC 2004)"
        );
        assert_eq!(
            seed.focus_reporting, host.focus_reporting,
            "focus_reporting (DEC 1004)"
        );
    }
    use super::*;
    use crate::config::{BiDiMode, ConfigChange, TerminalConfig};
    use crate::terminal::{Rgb, TerminalBuilder};

    fn clean_terminal_with_defaults() -> (Terminal, TerminalConfig) {
        let mut terminal = Terminal::new(4, 16);
        let config = TerminalConfig::default();
        terminal.apply_config(&config);
        terminal.take_damage();
        (terminal, config)
    }

    #[test]
    fn bidi_reload_damages_static_cells_exactly_when_changed() {
        let (mut terminal, mut config) = clean_terminal_with_defaults();
        let before = terminal.damage_epoch();

        config.bidi.mode = if config.bidi.mode == BiDiMode::Disabled {
            BiDiMode::Implicit
        } else {
            BiDiMode::Disabled
        };
        let changes = terminal.apply_config(&config);
        assert!(changes.contains(&ConfigChange::BiDi));
        assert!(terminal.has_damage(), "static RTL cells need a repaint");
        assert_eq!(terminal.damage_epoch(), before + 1);

        terminal.take_damage();
        let unchanged = terminal.apply_config(&config);
        assert!(!unchanged.contains(&ConfigChange::BiDi));
        assert!(!terminal.has_damage(), "an identical reload stays a no-op");
    }

    #[test]
    fn ambiguous_width_reload_changes_new_text_without_reflowing_existing_cells() {
        const AMBIGUOUS: &str = "\u{00B7}"; // MIDDLE DOT, East Asian Width=A
        let (mut terminal, mut config) = clean_terminal_with_defaults();
        config.ambiguous_width_double = true;
        assert!(
            terminal
                .apply_config(&config)
                .contains(&ConfigChange::AmbiguousWidth)
        );
        terminal.process(AMBIGUOUS.as_bytes());
        assert!(terminal.grid().cell(0, 0).unwrap().is_wide());
        assert!(terminal.grid().is_wide_continuation_at(0, 1));
        terminal.take_damage();

        config.ambiguous_width_double = false;
        assert!(
            terminal
                .apply_config(&config)
                .contains(&ConfigChange::AmbiguousWidth)
        );
        assert!(
            !terminal.has_damage(),
            "policy reload must not pretend it reflowed existing geometry"
        );
        assert!(
            terminal.grid().cell(0, 0).unwrap().is_wide(),
            "the previously received glyph keeps its two-cell geometry"
        );

        terminal.process(AMBIGUOUS.as_bytes());
        assert_eq!(terminal.grid().cell(0, 2).unwrap().char(), '\u{00B7}');
        assert!(
            !terminal.grid().cell(0, 2).unwrap().is_wide(),
            "subsequent input uses the newly selected narrow policy"
        );
    }

    #[test]
    fn bidi_reload_reprojects_already_received_rtl_text() {
        let (mut terminal, mut config) = clean_terminal_with_defaults();
        terminal.process("\u{05D0}\u{05D1}\u{05D2}".as_bytes());
        let visual: Vec<char> = terminal.cell_frame(4, 16).cells[0]
            .iter()
            .take(3)
            .map(|cell| cell.ch)
            .collect();
        assert_eq!(visual, vec!['\u{05D2}', '\u{05D1}', '\u{05D0}']);
        terminal.take_damage();

        config.bidi.mode = BiDiMode::Disabled;
        let changes = terminal.apply_config(&config);
        assert!(changes.contains(&ConfigChange::BiDi));
        assert!(terminal.has_damage());
        let logical: Vec<char> = terminal.cell_frame(4, 16).cells[0]
            .iter()
            .take(3)
            .map(|cell| cell.ch)
            .collect();
        assert_eq!(logical, vec!['\u{05D0}', '\u{05D1}', '\u{05D2}']);
    }

    fn settle(term: &mut Terminal) -> u64 {
        term.take_damage();
        assert!(!term.has_damage());
        term.damage_epoch()
    }

    #[test]
    fn cursor_color_only_reload_marks_visual_damage() {
        let mut term = Terminal::new(2, 8);
        let before = settle(&mut term);
        let mut config = TerminalConfig::default();
        config.cursor_color = Some(Rgb::new(0x12, 0x34, 0x56));

        let changes = term.apply_config(&config);

        assert!(changes.contains(&ConfigChange::CursorColor));
        assert!(term.has_damage(), "an idle cursor must be recolored now");
        assert!(term.damage_epoch() > before);
    }

    #[test]
    fn style_policy_reload_repaints_preexisting_cells() {
        let mut term = Terminal::new(2, 8);
        term.process(b"\x1b[1;33mB\x1b[0m \x1b[2mD");
        let before_cells = term.render_row(0);
        let before_epoch = settle(&mut term);
        let mut config = TerminalConfig::default();
        config.bold_is_bright = false;
        config.faint_opacity = 1.0;

        let changes = term.apply_config(&config);
        let after_cells = term.render_row(0);

        assert!(changes.contains(&ConfigChange::StylePolicy));
        assert_ne!(after_cells[0].fg, before_cells[0].fg);
        assert_ne!(after_cells[2].fg, before_cells[2].fg);
        assert!(term.has_damage());
        assert!(term.damage_epoch() > before_epoch);

        term.take_damage();
        let unchanged = term.apply_config(&config);
        assert!(!unchanged.contains(&ConfigChange::StylePolicy));
        assert!(!term.has_damage(), "an identical reload stays a no-op");
    }

    #[test]
    fn config_reload_during_reflow_survives_terminal_reset() {
        let mut term = TerminalBuilder::new()
            .size(6, 40)
            .tiered_scrollback_defaults()
            .build();
        for i in 0..200 {
            term.process(format!("line-{i}\r\n").as_bytes());
        }
        let initial_budget = term
            .main_grid()
            .scrollback_memory_budget()
            .expect("tiered store budget");
        let job = term
            .resize_offloading_scrollback(6, 20)
            .expect("width change detaches tiered store");

        let config = TerminalConfig {
            memory_budget: initial_budget.saturating_sub(1_000_000).max(1),
            scrollback_limit: None,
            ..TerminalConfig::default()
        };
        let changes = term.apply_config(&config);
        assert!(changes.contains(&ConfigChange::MemoryBudget));
        assert!(
            changes.contains(&ConfigChange::ScrollbackLimit),
            "detached finite limit must not be mistaken for unlimited"
        );
        assert_eq!(
            term.main_grid().scrollback_memory_budget(),
            Some(config.memory_budget)
        );
        assert_eq!(term.scrollback_line_limit(), None);
        let changes = term.apply_config(&config);
        assert!(
            !changes.contains(&ConfigChange::MemoryBudget),
            "pending budget makes an identical reload a no-op"
        );
        assert!(
            !changes.contains(&ConfigChange::ScrollbackLimit),
            "pending unlimited value makes an identical reload a no-op"
        );

        term.reset();
        assert!(
            term.finish_resize_offload(job.reflow()).is_none(),
            "the erase/replacement race never converges (RFL-3): nothing stale to rewrap"
        );

        assert_eq!(term.main_grid().scrollback_lines(), 0);
        assert_eq!(term.scrollback_line_limit(), None);
        assert_eq!(
            term.scrollback()
                .expect("cleared store re-attached")
                .memory_budget(),
            config.memory_budget
        );

        let changes = term.apply_config(&config);
        assert!(!changes.contains(&ConfigChange::MemoryBudget));
        assert!(!changes.contains(&ConfigChange::ScrollbackLimit));
    }

    #[test]
    fn config_reload_during_reflow_targets_saved_primary_under_alt_screen() {
        let mut term = TerminalBuilder::new()
            .size(6, 40)
            .tiered_scrollback_defaults()
            .build();
        for i in 0..100 {
            term.process(format!("main-{i}\r\n").as_bytes());
        }
        term.process(b"\x1b[?1049h");
        let initial_budget = term
            .main_grid()
            .scrollback_memory_budget()
            .expect("saved primary has tiered scrollback");
        let job = term
            .resize_offloading_scrollback(6, 20)
            .expect("saved primary store detaches");

        let config = TerminalConfig {
            memory_budget: initial_budget.saturating_sub(2_000_000).max(1),
            scrollback_limit: Some(50),
            ..TerminalConfig::default()
        };
        let changes = term.apply_config(&config);
        assert!(changes.contains(&ConfigChange::MemoryBudget));
        assert!(changes.contains(&ConfigChange::ScrollbackLimit));
        assert_eq!(
            term.main_grid().scrollback_memory_budget(),
            Some(config.memory_budget)
        );
        assert_eq!(term.scrollback_line_limit(), Some(50));

        let changes = term.apply_config(&config);
        assert!(!changes.contains(&ConfigChange::MemoryBudget));
        assert!(!changes.contains(&ConfigChange::ScrollbackLimit));

        assert!(
            term.finish_resize_offload(job.reflow()).is_none(),
            "widths agree — no convergence pass expected"
        );
        assert_eq!(
            term.main_grid().scrollback_memory_budget(),
            Some(config.memory_budget)
        );
        assert_eq!(term.scrollback_line_limit(), Some(50));
        term.process(b"\x1b[?1049l");
        assert_eq!(term.scrollback_line_limit(), Some(50));
        assert_eq!(
            term.scrollback()
                .expect("primary store restored")
                .memory_budget(),
            config.memory_budget
        );
    }

    #[test]
    fn a_config_reload_leaves_an_app_negotiated_bracketed_paste_alone() {
        let mut term = Terminal::new(4, 16);
        let config = TerminalConfig::default();
        term.apply_config(&config);
        term.process(b"\x1b[?2004h");
        assert!(term.modes().bracketed_paste(), "the app set DEC 2004");

        let changes = term.apply_config(&config);

        assert!(
            term.modes().bracketed_paste(),
            "an unrelated config reload must not clear the shell's bracketed paste"
        );
        assert!(!changes.contains(&ConfigChange::BracketedPaste));
        assert_eq!(
            term.format_paste("a\nb"),
            b"\x1b[200~a\rb\x1b[201~",
            "pastes stay framed after the reload"
        );
    }

    #[test]
    fn a_config_reload_leaves_app_negotiated_wrap_and_focus_reporting_alone() {
        let mut term = Terminal::new(4, 16);
        let config = TerminalConfig::default();
        term.apply_config(&config);
        term.process(b"\x1b[?7l\x1b[?1004h");
        assert!(!term.modes().auto_wrap(), "the app reset DECAWM");
        assert!(term.focus_reporting_enabled(), "the app set DEC 1004");

        let changes = term.apply_config(&config);

        assert!(
            !term.modes().auto_wrap(),
            "reload must not re-enable DECAWM"
        );
        assert!(
            term.focus_reporting_enabled(),
            "reload must not stop the app's focus reports"
        );
        assert!(!changes.contains(&ConfigChange::AutoWrap));
        assert!(!changes.contains(&ConfigChange::FocusReporting));
    }

    #[test]
    fn a_real_host_change_to_a_negotiated_mode_still_reaches_the_terminal() {
        let mut term = Terminal::new(4, 16);
        let mut config = TerminalConfig::default();
        term.apply_config(&config);
        term.process(b"\x1b[?2004h");

        config.bracketed_paste = false;
        config.auto_wrap = false;
        config.focus_reporting = true;
        let changes = term.apply_config(&config);
        assert!(changes.contains(&ConfigChange::AutoWrap));
        assert!(changes.contains(&ConfigChange::FocusReporting));
        assert!(!changes.contains(&ConfigChange::BracketedPaste));
        assert!(!term.modes().auto_wrap());
        assert!(term.focus_reporting_enabled());
        assert!(term.modes().bracketed_paste());

        config.bracketed_paste = true;
        assert!(
            term.apply_config(&config).is_empty(),
            "the mode already matches the new host value, so nothing was modified"
        );
        config.bracketed_paste = false;
        let changes = term.apply_config(&config);
        assert!(changes.contains(&ConfigChange::BracketedPaste));
        assert!(!term.modes().bracketed_paste());
        assert_eq!(term.format_paste("a\nb"), b"a\rb", "no guards to emit");

        assert!(term.apply_config(&config).is_empty());
    }

    #[test]
    fn a_reset_returns_a_negotiated_mode_to_the_power_on_value_not_the_config_seed() {
        let mut term = Terminal::new(4, 16);
        let mut config = TerminalConfig::default();
        config.bracketed_paste = true;
        term.apply_config(&config);
        assert!(term.modes().bracketed_paste(), "the host seeded 2004 on");

        term.process(b"\x1b[!p"); // DECSTR
        assert!(!term.modes().bracketed_paste(), "soft reset clears 2004");
        assert!(
            !term
                .apply_config(&config)
                .contains(&ConfigChange::BracketedPaste),
            "an unchanged config does not undo the program's reset"
        );
        assert!(!term.modes().bracketed_paste());

        term.process(b"\x1b[?2004h");
        term.reset(); // RIS
        assert!(!term.modes().bracketed_paste(), "RIS clears 2004");
        assert!(
            !term
                .apply_config(&config)
                .contains(&ConfigChange::BracketedPaste)
        );
        assert!(!term.modes().bracketed_paste());
    }

    const fn rgb(r: u8, g: u8, b: u8) -> Rgb {
        Rgb { r, g, b }
    }

    fn seeded_baseline(seed: u8) -> ColorBaseline {
        let mut palette = aterm_types::ColorPalette::new();
        for index in 0..16u8 {
            palette.set(index, rgb(seed, index, seed.wrapping_add(index)));
        }
        ColorBaseline {
            foreground: rgb(seed, seed, seed),
            background: rgb(seed / 2, 0, seed / 3),
            cursor: Some(rgb(0, seed, 0)),
            selection_background: Some(rgb(seed, 0, 0)),
            selection_foreground: None,
            palette: Some(palette),
            color_scheme: Some(aterm_types::Appearance::Dark),
        }
    }

    const PROGRAM_BG: Rgb = rgb(0x10, 0x11, 0x12);
    const PROGRAM_P1: Rgb = rgb(0x28, 0x2a, 0x36);

    #[test]
    fn rebase_keeps_program_colours_and_moves_the_reset_baseline() {
        let mut t = Terminal::new(4, 16);
        t.set_allow_palette_reconfigure(true);
        t.rebase_colors(&seeded_baseline(200));
        t.process(b"\x1b]11;rgb:10/11/12\x07\x1b]4;1;rgb:28/2a/36\x07");
        let new = seeded_baseline(90);
        let rebase = t.rebase_colors(&new);
        assert!(rebase.colors_changed, "fg and untouched slots moved");
        assert_eq!(t.default_background(), PROGRAM_BG, "OSC 11 survives");
        assert_eq!(t.palette_color(1), PROGRAM_P1, "OSC 4;1 survives");
        assert_eq!(t.default_foreground(), new.foreground, "fg follows");
        let new_palette = new.palette.as_ref().expect("seeded palette");
        assert_eq!(t.palette_color(2), new_palette.get(2), "slot 2 follows");
        assert_eq!(t.color_baseline(), new);

        t.process(b"\x1b]111\x07\x1b]104\x07");
        assert_eq!(t.default_background(), new.background, "OSC 111 → new bg");
        assert_eq!(
            t.palette_color(1),
            new_palette.get(1),
            "OSC 104 → new palette"
        );

        let mut legacy = Terminal::new(4, 16);
        legacy.rebase_colors(&seeded_baseline(200));
        legacy.process(b"\x1b]11;rgb:10/11/12\x07");
        legacy.set_default_background(new.background);
        assert_ne!(legacy.default_background(), PROGRAM_BG);
    }

    #[test]
    fn rebase_reports_change_only_when_live_colours_move() {
        let mut t = Terminal::new(4, 16);
        t.rebase_colors(&seeded_baseline(200));
        t.take_damage();
        let same = t.color_baseline();
        assert_eq!(t.rebase_colors(&same), ColorRebase::default());
        assert!(!t.has_damage(), "a no-op rebase leaves no damage");
        let moved = t.rebase_colors(&seeded_baseline(90));
        assert!(moved.colors_changed && moved.cursor_changed);
        assert!(t.has_damage());
    }

    #[test]
    fn rebase_reports_the_scheme_once_after_the_colours() {
        let mut t = Terminal::new(4, 16);
        t.process(b"\x1b[?2031h");
        while t.take_response().is_some() {}
        let mut light = seeded_baseline(240);
        light.color_scheme = Some(aterm_types::Appearance::Light);
        let rebase = t.rebase_colors(&light);
        assert!(rebase.scheme_changed);
        assert_eq!(t.color_scheme(), aterm_types::Appearance::Light);
        assert_eq!(t.take_response().as_deref(), Some(&b"\x1b[?997;2n"[..]));
        assert!(!t.rebase_colors(&light).scheme_changed);
        assert_eq!(t.take_response(), None);
    }

    #[test]
    fn apply_config_colour_half_is_rebase() {
        let configs = [
            TerminalConfig::default(),
            TerminalConfig {
                default_foreground: rgb(1, 2, 3),
                default_background: rgb(250, 250, 240),
                cursor_color: Some(rgb(9, 9, 9)),
                color_scheme: Some(aterm_types::Appearance::Light),
                ..TerminalConfig::default()
            },
            TerminalConfig {
                selection_background: Some(rgb(5, 6, 7)),
                selection_foreground: Some(rgb(8, 9, 10)),
                custom_palette: seeded_baseline(77).palette,
                allow_palette_reconfigure: true,
                ..TerminalConfig::default()
            },
        ];
        let mut a = Terminal::new(4, 16);
        let mut b = Terminal::new(4, 16);
        for t in [&mut a, &mut b] {
            t.process(b"\x1b]11;rgb:10/11/12\x07\x1b]12;rgb:fe/01/80\x07");
        }
        let mut saw_change = false;
        for config in &configs {
            b.set_allow_palette_reconfigure(config.allow_palette_reconfigure);
            let changes = a.apply_config(config);
            let rebase = b.rebase_colors(&ColorBaseline::of_config(config));
            assert_eq!(
                changes.contains(&ConfigChange::Colors),
                rebase.colors_changed
            );
            assert_eq!(
                changes.contains(&ConfigChange::CursorColor),
                rebase.cursor_changed
            );
            assert_eq!(
                changes.contains(&ConfigChange::ColorScheme),
                rebase.scheme_changed
            );
            saw_change |= rebase.colors_changed;
            assert_eq!(a.color_baseline(), b.color_baseline());
            assert_eq!(a.default_background(), b.default_background());
            assert_eq!(a.default_foreground(), b.default_foreground());
            assert_eq!(a.cursor_color(), b.cursor_color());
            assert_eq!(a.color_scheme(), b.color_scheme());
            for i in 0..=u8::MAX {
                assert_eq!(a.palette_color(i), b.palette_color(i), "slot {i}");
            }
        }
        assert!(saw_change, "the configs must move the colours");
    }

    #[test]
    fn apply_config_colors_takes_the_colours_and_policy_and_leaves_the_programs_modes() {
        use crate::terminal::CursorStyle;
        let config = TerminalConfig {
            default_background: rgb(250, 250, 240),
            bold_is_bright: false,
            auto_wrap: false,
            ..TerminalConfig::default()
        };
        let program = |t: &mut Terminal| t.process(b"\x1b[?25l\x1b[6 q");
        let modes = |t: &Terminal| (t.cursor_visible(), t.cursor_style(), t.modes().auto_wrap);
        let mut colors = Terminal::new(4, 16);
        program(&mut colors);
        let set = modes(&colors);
        assert_eq!(set, (false, CursorStyle::SteadyBar, true), "fixture");
        colors.take_damage();
        colors.apply_config_colors(&config);
        assert_eq!(modes(&colors), set, "the program's modes stay");
        assert_eq!(colors.default_background(), rgb(250, 250, 240));
        assert!(!colors.color.bold_is_bright, "the colour policy is taken");
        assert!(colors.has_damage(), "a colour move repaints");
        let mut whole = Terminal::new(4, 16);
        program(&mut whole);
        whole.apply_config(&config);
        assert_ne!(
            modes(&whole),
            set,
            "negative control: the whole config moves the program's modes"
        );
    }
}
