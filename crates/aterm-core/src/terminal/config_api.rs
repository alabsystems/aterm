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

    /// The configured `(foreground, background)`: the pair the host's theme or config set, which
    /// a program's OSC 10/11 (and their resets) and DECSCNM never move, read without cloning the
    /// palette [`Self::color_baseline`] carries.
    #[inline]
    #[must_use]
    pub const fn configured_colors(&self) -> (aterm_types::Rgb, aterm_types::Rgb) {
        (
            self.color.configured_foreground,
            self.color.configured_background,
        )
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
    pub fn apply_config(
        &mut self,
        config: &crate::config::TerminalConfig,
    ) -> Vec<crate::config::ConfigChange> {
        self.apply_config_shared(std::sync::Arc::new(config.clone()))
    }

    /// [`Self::apply_config`] of a config the host shares, journaled as one [`super::HostOp::Config`] so a recording replays the install where it ran ([`Self::host_journal_since`]).
    pub fn apply_config_shared(
        &mut self,
        config: std::sync::Arc<crate::config::TerminalConfig>,
    ) -> Vec<crate::config::ConfigChange> {
        let (changes, recolored) = self.install_config(&config);
        self.host_journal.push(super::HostOp::Config {
            config: super::HostConfig(config),
            recolored,
        });
        changes
    }

    /// What [`Self::apply_config`] changes, journaling nothing: the aspects that changed, and whether what the screen draws moved (the cursor colour, another colour or the style policy).
    #[allow(
        clippy::too_many_lines,
        reason = "flat field-by-field config application"
    )]
    fn install_config(
        &mut self,
        config: &crate::config::TerminalConfig,
    ) -> (Vec<crate::config::ConfigChange>, bool) {
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

        (changes, visual_repaint)
    }

    /// [`Self::apply_config`] for an engine a checkpoint restored: it takes `config`'s colour baseline, default cursor style, mode seeds, budgets and policies, keeps every mode and the history limit the restore installed, and leaves any colour-scheme report the rebase queued for the caller to drain. It sets a replay up, so it journals nothing.
    pub fn apply_config_keeping_modes(&mut self, config: &crate::config::TerminalConfig) {
        let modes = self.modes;
        let limit = self.scrollback_line_limit();
        let _ = self.install_config(config);
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
#[path = "config_api_tests.rs"]
mod tests;
