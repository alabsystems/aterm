// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Configuration hot-reload API for Terminal.
//!
//! Contains `apply_config()`.
//! Extracted from mod.rs to reduce file size.

use super::Terminal;

impl Terminal {
    // =========================================================================
    // Configuration Hot-Reload API
    // =========================================================================

    /// Apply configuration changes to the terminal.
    ///
    /// This method allows runtime modification of terminal settings without
    /// recreating the terminal instance. It returns a list of configuration
    /// aspects that were changed, which can be used for efficient UI updates.
    ///
    /// # Example
    ///
    /// ```
    /// use aterm_core::config::{ConfigChange, TerminalConfig};
    /// use aterm_core::terminal::{CursorStyle, Terminal};
    ///
    /// let mut term = Terminal::new(24, 80);
    ///
    /// // Create new configuration (cursor_blink=true differs from terminal default false)
    /// let mut config = TerminalConfig::default();
    /// config.cursor_style = CursorStyle::SteadyBar;
    /// config.cursor_blink = true;
    ///
    /// // Apply and check what changed
    /// let changes = term.apply_config(&config);
    /// assert!(changes.contains(&ConfigChange::CursorStyle));
    /// assert!(changes.contains(&ConfigChange::CursorBlink));
    /// ```
    ///
    /// # Change Detection
    ///
    /// The method only applies changes for settings that differ from the
    /// current terminal state. The returned `Vec<ConfigChange>` contains
    /// only the settings that were actually modified.
    ///
    /// # Settings Applied
    ///
    /// - **Cursor**: style, blink, color, visibility
    /// - **Font**: descriptor (family, size, weight, italic)
    /// - **Colors**: foreground, background, cursor, selection background/
    ///   foreground, palette
    /// - **Modes**: auto-wrap, focus reporting, bracketed paste
    /// - **Text policy**: ambiguous width, style attributes, BiDi projection
    /// - **Performance**: memory budget, total scrollback line limit, and
    ///   synchronized-output timeout
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
        // Render-time configuration can change the pixels of already-stored
        // cells without writing the grid. Fold every such change into one
        // damage decision at the end so damage-keyed frontends cannot retain
        // an old frame after a config reload.
        let mut visual_repaint = false;

        // Cursor style. apply_config is host configuration (not an app escape), so it
        // also persists the DEFAULT — otherwise a config-set cursor would revert to
        // BlinkingBlock on the next RIS/DECSTR (parity with set_default_cursor_style).
        self.default_cursor_style = config.cursor_style;
        if self.modes.cursor_style != config.cursor_style {
            self.modes.cursor_style = config.cursor_style;
            changes.push(ConfigChange::CursorStyle);
        }

        // Cursor blink
        if self.modes.cursor_blink != config.cursor_blink {
            self.modes.cursor_blink = config.cursor_blink;
            changes.push(ConfigChange::CursorBlink);
        }

        // Cursor color. Always retain the host baseline so OSC 112/RIS restore
        // the configured theme cursor instead of silently falling back to fg.
        if self.color.cursor_color != config.cursor_color {
            self.color.cursor_color = config.cursor_color;
            changes.push(ConfigChange::CursorColor);
            visual_repaint = true;
        }
        self.color.configured_cursor = config.cursor_color;
        self.color.frame_cursor_authoritative = true;

        // Cursor visibility
        if self.modes.cursor_visible != config.cursor_visible {
            self.modes.cursor_visible = config.cursor_visible;
            changes.push(ConfigChange::CursorVisible);
        }

        // Font descriptor
        if self.font != config.font {
            self.font = config.font.clone();
            changes.push(ConfigChange::Font);
        }

        // Default foreground
        let fg_changed = self.color.default_foreground != config.default_foreground;
        if fg_changed {
            self.color.default_foreground = config.default_foreground;
        }
        // Always update configured defaults so OSC 110/111 reset to theme
        // colors, not hardcoded constants (#7443).
        self.color.configured_foreground = config.default_foreground;
        self.color.frame_foreground_authoritative = true;

        // Default background
        let bg_changed = self.color.default_background != config.default_background;
        if bg_changed {
            self.color.default_background = config.default_background;
        }
        // Always update configured defaults so OSC 110/111 reset to theme
        // colors, not hardcoded constants (#7443).
        self.color.configured_background = config.default_background;
        self.color.frame_background_authoritative = true;

        // Selection background
        let sel_bg_changed = self.color.selection_background != config.selection_background;
        if sel_bg_changed {
            self.color.selection_background = config.selection_background;
        }
        // OSC 117/RIS reset selection highlighting to the host theme, not an
        // unrelated hardcoded/default-background color.
        self.color.configured_selection_background = config.selection_background;
        let sel_fg_changed = self.color.selection_foreground != config.selection_foreground;
        if sel_fg_changed {
            self.color.selection_foreground = config.selection_foreground;
        }
        self.color.configured_selection_foreground = config.selection_foreground;

        // Custom palette
        // Always update configured_palette so OSC 104 resets to theme
        // colors, not hardcoded xterm defaults (matching #7443 pattern
        // for configured_foreground/configured_background).
        self.color
            .configured_palette
            .clone_from(&config.custom_palette);

        let palette_changed = if let Some(ref palette) = config.custom_palette {
            if self.color.palette == *palette {
                false
            } else {
                self.color.palette = palette.clone();
                true
            }
        } else if self.color.palette.overrides_count() > 0 {
            // config.custom_palette is None — reset any active overrides
            // back to built-in defaults so the palette can be reverted.
            self.color.palette.reset();
            true
        } else {
            false
        };

        if fg_changed || bg_changed || sel_bg_changed || sel_fg_changed || palette_changed {
            changes.push(ConfigChange::Colors);
            visual_repaint = true;
        }

        // Auto-wrap mode
        if self.modes.auto_wrap != config.auto_wrap {
            self.modes.auto_wrap = config.auto_wrap;
            changes.push(ConfigChange::AutoWrap);
        }

        // Focus reporting
        if self.modes.focus_reporting != config.focus_reporting {
            self.modes.focus_reporting = config.focus_reporting;
            changes.push(ConfigChange::FocusReporting);
        }

        // Bracketed paste
        if self.modes.bracketed_paste != config.bracketed_paste {
            self.modes.bracketed_paste = config.bracketed_paste;
            changes.push(ConfigChange::BracketedPaste);
        }

        // OSC 52 clipboard query policy — sync both the mirror bit on
        // `modes` and the authoritative `clipboard_auth` capability state
        // (#7874, #7878 CF-005).
        if self.modes.allow_osc52_query != config.allow_osc52_query {
            self.modes.allow_osc52_query = config.allow_osc52_query;
            if config.allow_osc52_query {
                self.clipboard_auth.authorize_query();
            } else {
                self.clipboard_auth.revoke_query();
            }
            changes.push(ConfigChange::Osc52ClipboardQuery);
        }

        // CSI t window manipulation policy (#7139)
        if self.modes.allow_window_ops != config.allow_window_ops {
            self.modes.allow_window_ops = config.allow_window_ops;
            changes.push(ConfigChange::WindowOps);
        }

        // Desktop notification policy (OSC 9/99/777) — #7878 CF-009.
        if self.modes.allow_notifications != config.allow_notifications {
            self.modes.allow_notifications = config.allow_notifications;
            changes.push(ConfigChange::Notifications);
        }

        // OSC 4 / OSC 21 indexed palette reconfigure policy — #7937 F01-3.
        if self.modes.allow_palette_reconfigure != config.allow_palette_reconfigure {
            self.modes.allow_palette_reconfigure = config.allow_palette_reconfigure;
            changes.push(ConfigChange::PaletteReconfigure);
        }

        // Ambiguous-width character mode
        if self.modes.ambiguous_width_double != config.ambiguous_width_double {
            self.modes.ambiguous_width_double = config.ambiguous_width_double;
            changes.push(ConfigChange::AmbiguousWidth);
        }

        // Style-attribute policy (W5): bold-to-bright promotion + faint opacity.
        // This is resolved at render time, so existing cells do not need to be
        // rewritten. They DO need to be repainted: without full damage the
        // frontend's damage-epoch early-out can swallow a style-only reload.
        let new_faint = if config.faint_opacity.is_finite() {
            config.faint_opacity.clamp(0.0, 1.0)
        } else {
            aterm_types::DIM_FACTOR
        };
        if self.color.bold_is_bright != config.bold_is_bright
            || (self.color.faint_opacity - new_faint).abs() > f32::EPSILON
        {
            self.color.bold_is_bright = config.bold_is_bright;
            self.color.faint_opacity = new_faint;
            changes.push(ConfigChange::StylePolicy);
            visual_repaint = true;
        }

        // Memory budget
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
            // set_memory_budget clamps display_offset internally (#7233).
            changes.push(ConfigChange::MemoryBudget);
        }

        // BiDi configuration
        // Sync both the config storage and the terminal modes
        if self.bidi_state.config != config.bidi {
            self.bidi_state.config = config.bidi.clone();
            // Sync config settings to terminal modes (escape sequences can override later)
            self.modes.bidi_mode = config.bidi.mode;
            self.modes.bidi_direction = config.bidi.direction;
            // Note: bidi_box_mirroring, bidi_autodetection, bidi_arrow_swap are
            // escape-sequence-only modes (DEC ?2500, ?2501, ?1243) with no config equivalent
            self.invalidate_bidi_all();
            changes.push(ConfigChange::BiDi);
        }

        // Scrollback limit - apply immediately by truncating if needed. Routed
        // through the Terminal-level setter so it reaches the PRIMARY-content
        // grid (never the alt buffer's spec'd zero-scrollback ring) and clamps
        // display_offset itself.
        let current_limit = self.scrollback_line_limit();
        let new_limit = config.scrollback_limit;
        if current_limit != new_limit {
            self.set_scrollback_line_limit(new_limit);
            changes.push(ConfigChange::ScrollbackLimit);
        }

        // Sync timeout
        let new_timeout = std::time::Duration::from_millis(config.sync_timeout_ms);
        if self.sync_timeout_duration != new_timeout {
            self.sync_timeout_duration = new_timeout;
            changes.push(ConfigChange::SyncTimeout);
        }

        if visual_repaint {
            // A config-driven recolor/policy change repaints already-drawn cells
            // without changing their content. Advance full-grid damage so a
            // frontend keyed by `damage_epoch` cannot swallow the reload.
            self.grid.damage_mut().mark_full();
        }

        changes
    }
}

#[cfg(test)]
mod tests {
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
    fn style_policy_reload_damages_static_cells_exactly_when_changed() {
        let (mut terminal, mut config) = clean_terminal_with_defaults();
        let before = terminal.damage_epoch();

        config.bold_is_bright = !config.bold_is_bright;
        let changes = terminal.apply_config(&config);
        assert!(changes.contains(&ConfigChange::StylePolicy));
        assert!(terminal.has_damage(), "static styled cells need a repaint");
        assert_eq!(terminal.damage_epoch(), before + 1);

        terminal.take_damage();
        let unchanged = terminal.apply_config(&config);
        assert!(!unchanged.contains(&ConfigChange::StylePolicy));
        assert!(!terminal.has_damage(), "an identical reload stays a no-op");
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

    #[cfg(feature = "bidi")]
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
    }

    #[test]
    fn bidi_config_reload_marks_existing_rows_for_repaint() {
        let mut term = Terminal::new(2, 8);
        term.process("\u{05D0}\u{05D1}\u{05D2}".as_bytes());
        let before = settle(&mut term);
        let mut config = TerminalConfig::default();
        config.bidi.direction = aterm_types::ParagraphDirection::Ltr;

        let changes = term.apply_config(&config);

        assert!(changes.contains(&ConfigChange::BiDi));
        assert!(term.has_damage());
        assert!(term.damage_epoch() > before);
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

        // RIS/host reset erases scrollback while the worker owns the old store.
        // It must neither resurrect old history nor discard host settings.
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

        // A second identical reload must be a true no-op after reconciliation.
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
}
