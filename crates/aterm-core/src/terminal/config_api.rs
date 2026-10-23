// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Configuration hot-reload: `Terminal::apply_config`, and its colour half
//! `Terminal::rebase_colors`.

use super::Terminal;

/// Push one host-config value into a mode an application also negotiates over
/// the wire, and report whether the live mode moved.
///
/// Diffs against `seed`, the host value last applied
/// ([`ConfiguredModes`](super::state::ConfiguredModes)), never against `live`:
/// a live value that disagrees usually means the program set it, and diffing
/// live would re-assert the power-on value on every reload (for mode 2004,
/// stripping a shell's bracketed-paste guards).
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

/// The live colours `apply_config` judges a change on, before and after: the
/// cursor ([`ConfigChange::CursorColor`]) and the rest
/// ([`ConfigChange::Colors`]).
///
/// [`ConfigChange::CursorColor`]: crate::config::ConfigChange::CursorColor
/// [`ConfigChange::Colors`]: crate::config::ConfigChange::Colors
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

/// The host's colour baseline: what OSC 104/110-119 and RIS reset to, and the
/// scheme DSR 996 reports while no program holds its own background. Read it
/// with [`Terminal::color_baseline`] and move it with [`Terminal::rebase_colors`].
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
    /// The baseline a [`TerminalConfig`](crate::config::TerminalConfig) declares.
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

/// What a [`Terminal::rebase_colors`] changed, judged on the live colours.
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
    // Configuration Hot-Reload API

    /// The host's colour baseline now: the configured colours, palette and
    /// declared scheme that OSC 104/110-119 and RIS reset to.
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

    /// Move the theme baseline and keep a program's colours (OSC 4/10/11/12/
    /// 17/19/21) as diffs on top: the colour half of
    /// [`apply_config`](Self::apply_config), for hosts that install a theme
    /// without a whole [`TerminalConfig`](crate::config::TerminalConfig) (the
    /// web embeds). OSC 104/110-119 and RIS then reset to the new baseline,
    /// slots the program never touched follow it, and a program's palette diff
    /// is replayed only under
    /// [`allow_palette_reconfigure`](Self::allow_palette_reconfigure). The
    /// reported scheme is reconciled once, after the colours (at most one
    /// `CSI ? 997` push). Marks full damage when any live colour moved.
    #[inline]
    pub fn rebase_colors(&mut self, baseline: &ColorBaseline) -> ColorRebase {
        let rebase = self.rebase_colors_inner(baseline, self.modes.allow_palette_reconfigure);
        if rebase.cursor_changed || rebase.colors_changed {
            self.grid.damage_mut().mark_full();
        }
        rebase
    }

    /// [`Self::rebase_colors`] without the damage, under the palette gate the
    /// caller is about to install (`apply_config` passes its config's).
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
        // The reported scheme is judged once, on the new baseline, the new
        // configured colours and the replayed overrides together: no transient
        // value, at most one push.
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
    /// use aterm_core::config::{ConfigChange, TerminalConfig};
    /// use aterm_core::terminal::{CursorStyle, Terminal};
    ///
    /// # Change detection
    ///
    /// Most settings apply when they differ from the current state. The
    /// program-negotiated modes (`auto_wrap`/DECAWM, `bracketed_paste`/DEC 2004,
    /// `focus_reporting`/DEC 1004) are instead diffed against the host's last
    /// applied value, so a live mode moves only when the host's own value moves
    /// and an unrelated reload never strips a shell's paste guards. See
    /// `apply_negotiated_mode`.
    ///
    /// A program's colours (OSC 4/10/11/12/17/19/21) ride over the configured
    /// ones and are replayed after the new baseline lands; a palette diff is
    /// replayed only under `allow_palette_reconfigure`, so closing the gate
    /// puts the theme palette back in the same call.
    ///
    /// # Settings applied
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

        // Cursor style. Host configuration, so it also persists the default a
        // later RIS/DECSTR restores. `CursorStyle` carries the blink bit the
        // renderer and DEC mode 12 use, so the style is applied verbatim and
        // `modes.cursor_blink` follows it below.
        self.default_cursor_style = config.cursor_style;
        if self.modes.cursor_style != config.cursor_style {
            self.modes.cursor_style = config.cursor_style;
            changes.push(ConfigChange::CursorStyle);
        }

        // Cursor blink — the blink bit of the style applied just above, not a
        // second flag beside it. Kept as a mirror so `TerminalModes::cursor_blink`
        // and DEC mode 12 cannot drift from the cursor the user is looking at.
        if self.modes.cursor_blink != self.modes.cursor_style.blinks() {
            self.modes.cursor_blink = self.modes.cursor_style.blinks();
            changes.push(ConfigChange::CursorBlink);
        }

        // Colours: the theme is the BASELINE, and a program's colours ride over
        // it. Take the program's diff from the old baseline (OSC 4/10/11/12/17/
        // 19/21 sets — what `capture_color_repr` carries across a handoff),
        // move every baseline and set live = baseline, then replay the diff.
        // So OSC 104/110-119 and RIS reset to the new theme, slots the program
        // never touched follow it, and a reload (any key, a theme-folder edit,
        // an OS appearance flip) preserves a base16 palette or an editor's
        // OSC 11 background. The diff's palette entries are replayed only while
        // the host allows palette sets, so turning the gate off takes a
        // program's palette out of every live session at once; its fg/bg/
        // cursor/selection sets are ungated and stay. The kitty stack holds
        // diffs too, so it needs no rebase (a pop under a closed gate skips
        // the palette). Change and damage are judged on the live colours.
        // This is [`Self::rebase_colors`], with the gate this config installs;
        // damage stays folded into the one decision below.
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

        // Cursor visibility
        if self.modes.cursor_visible != config.cursor_visible {
            self.modes.cursor_visible = config.cursor_visible;
            changes.push(ConfigChange::CursorVisible);
        }

        if rebase.colors_changed {
            changes.push(ConfigChange::Colors);
            visual_repaint = true;
        }

        // The three APP-negotiated modes (DECAWM, focus reporting, bracketed
        // paste): each moves only when the host's value moved, so a reload for
        // an unrelated setting cannot re-assert the power-on value over what the
        // running program negotiated. See [`ConfiguredModes`].
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

        // OSC 52 clipboard query policy — sync both the mirror bit on
        // `modes` and the authoritative `clipboard_auth` capability state
        //.
        if self.modes.allow_osc52_query != config.allow_osc52_query {
            self.modes.allow_osc52_query = config.allow_osc52_query;
            if config.allow_osc52_query {
                self.clipboard_auth.authorize_query();
            } else {
                self.clipboard_auth.revoke_query();
            }
            changes.push(ConfigChange::Osc52ClipboardQuery);
        }

        // CSI t window manipulation policy
        if self.modes.allow_window_ops != config.allow_window_ops {
            self.modes.allow_window_ops = config.allow_window_ops;
            changes.push(ConfigChange::WindowOps);
        }

        // Desktop notification policy (OSC 9/99/777).
        if self.modes.allow_notifications != config.allow_notifications {
            self.modes.allow_notifications = config.allow_notifications;
            changes.push(ConfigChange::Notifications);
        }

        // OSC 4 / OSC 21 indexed palette reconfigure policy.
        if self.modes.allow_palette_reconfigure != config.allow_palette_reconfigure {
            self.set_allow_palette_reconfigure(config.allow_palette_reconfigure);
            changes.push(ConfigChange::PaletteReconfigure);
        }

        // Ambiguous-width character mode
        if self.modes.ambiguous_width_double != config.ambiguous_width_double {
            self.modes.ambiguous_width_double = config.ambiguous_width_double;
            changes.push(ConfigChange::AmbiguousWidth);
        }

        // Style-attribute policy: bold-to-bright promotion, faint opacity and
        // the theme-pair contrast floor. This is resolved at render time, so
        // existing cells do not need to be rewritten. They do need to be
        // repainted: without full damage the frontend's damage-epoch early-out
        // can swallow a style-only reload.
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
        if self.color.bold_is_bright != config.bold_is_bright
            || (self.color.faint_opacity - new_faint).abs() > f32::EPSILON
            || (self.color.indexed_minimum_contrast - new_floor).abs() > f32::EPSILON
        {
            self.color.bold_is_bright = config.bold_is_bright;
            self.color.faint_opacity = new_faint;
            self.color.indexed_minimum_contrast = new_floor;
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
            // set_memory_budget clamps display_offset internally.
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
        // through the Terminal-level setter so it reaches the primary-content
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

        // The focus-report gate reads DEC 1004 from the lock-free `ModeMirror`,
        // and apply_config can change it outside `process()`: republish.
        self.refresh_mode_mirror();

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

    /// `ConfiguredModes::default` must equal the modes a default
    /// `TerminalConfig` carries; otherwise the first `apply_config` of a
    /// default config would override modes the program negotiated.
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

    /// A config reload must not un-negotiate DEC 2004: bracketed paste belongs
    /// to the shell, and dropping it strips the `ESC[200~`/`ESC[201~` guards
    /// from later pastes while the shell believes it is protected.
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

    /// The other two modes an application negotiates over the wire — DECAWM and
    /// focus reporting (DEC 1004) — share the seam and the defect: a reload for
    /// an unrelated setting must not re-assert the power-on value over what a
    /// full-screen program asked for.
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

    /// The other half of the contract: a host that genuinely changes one of
    /// these settings still reaches the terminal, and still reports the change
    /// exactly once. Without it the seam would be indistinguishable from
    /// deleting the three settings outright.
    #[test]
    fn a_real_host_change_to_a_negotiated_mode_still_reaches_the_terminal() {
        let mut term = Terminal::new(4, 16);
        let mut config = TerminalConfig::default();
        term.apply_config(&config);
        term.process(b"\x1b[?2004h");

        // The host now wants pastes unframed, over the app's own request.
        config.bracketed_paste = false;
        config.auto_wrap = false;
        config.focus_reporting = true;
        // `bracketed_paste` held the same value in the previous config, so its
        // seed did not move and the app's mode 2004 survives; only the two
        // settings the host really changed are announced.
        let changes = term.apply_config(&config);
        assert!(changes.contains(&ConfigChange::AutoWrap));
        assert!(changes.contains(&ConfigChange::FocusReporting));
        assert!(!changes.contains(&ConfigChange::BracketedPaste));
        assert!(!term.modes().auto_wrap());
        assert!(term.focus_reporting_enabled());
        assert!(term.modes().bracketed_paste());

        // Now the host really does move mode 2004, and it lands.
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

        // An identical reload stays a no-op for all three.
        assert!(term.apply_config(&config).is_empty());
    }

    /// A reset returns these modes to the DEC power-on state, not the host's
    /// configured seed (as `handle_decstr` and RIS do), and does not resurrect
    /// the seed.
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

    /// A baseline whose every colour is derived from `seed`, so two seeds
    /// differ in every slot.
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

        // Negative control: the legacy setter (live = baseline) loses the
        // program's background.
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
        // Non-vacuity: a different baseline is reported and damages.
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
        // The same baseline again: no push.
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
}
