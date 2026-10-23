// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `App` glue for the in-window command palette ([`crate::palette`]).

use crate::App;
use crate::control_theme::PreviewHold;
use crate::palette::{
    NativeCommandScope, NativeCommandTarget, PaletteLive, PaletteState, PaletteTarget, PaletteTheme,
};

/// The `[keybindings]` action behind a menu action, if any: off macOS,
/// [`App::palette_live`] shows each menu row's chord from the effective binding table (seeds
/// plus the user's rebinds).
#[cfg(not(target_os = "macos"))]
fn menu_binding(action: crate::menu::MenuAction) -> Option<crate::keybinding::Action> {
    use crate::keybinding::Action as K;
    use crate::menu::MenuAction as M;
    Some(match action {
        M::About | M::Version => K::ToggleAbout,
        M::NewWindow => K::NewWindow,
        M::NewTab => K::NewTab,
        M::ReopenClosedTab => K::ReopenClosedTab,
        M::CloseTab => K::CloseTab,
        M::Copy => K::Copy,
        M::Paste => K::Paste,
        M::SelectAll => K::SelectAll,
        M::Find => K::Find,
        M::FindNext => K::FindNext,
        M::FindPrev => K::FindPrev,
        M::ToggleFullScreen => K::ToggleFullscreen,
        M::FontIncrease => K::FontIncrease,
        M::FontDecrease => K::FontDecrease,
        M::FontActualSize => K::FontReset,
        M::SplitVertical => K::SplitVertical,
        M::SplitHorizontal => K::SplitHorizontal,
        M::ToggleMatrixRain => K::ToggleMatrixRain,
        M::ToggleSeriousMode => K::ToggleSeriousMode,
        M::ToggleSettings => K::ToggleSettings,
        M::OpenPalette => K::OpenPalette,
        M::ChooseTheme => K::ChooseTheme,
        M::NextTab => K::NextTab,
        M::PrevTab => K::PrevTab,
        M::RenameSession => K::RenameSession,
        M::HarnessLog => K::OpenLedger,
        _ => return None,
    })
}

impl App {
    /// Snapshot command metadata for exactly the active native view in `wid`.
    fn native_palette_scope(&self, wid: crate::WindowId) -> Option<NativeCommandScope> {
        let (instance, view) = self.active_native_view(wid)?;
        let generation = self.native_runtime.view_generation(view)?;
        let app = self.native_runtime.app(instance)?;
        let mut commands = self.native_runtime.commands(instance, view).ok()?;
        commands
            .retain(|command| !matches!(command.id.as_str(), "editor/find" | "settings/search"));
        Some(NativeCommandScope {
            window: wid,
            instance,
            view,
            generation,
            section: format!("{} App", app.descriptor().name),
            commands,
        })
    }

    /// Build a resolved [`PaletteSnapshot`] for window `wid`.
    pub(crate) fn palette_snapshot(&self, wid: crate::WindowId) -> PaletteState {
        let mut state = PaletteState::new();
        if let Some(scope) = self.native_palette_scope(wid) {
            state = state.with_native_commands(scope);
        }
        state = state.with_themes(self.palette_themes());
        state.resolve(&self.palette_live_for_window(Some(wid)));
        state
    }

    /// The theme the configuration draws: what a standing preview will restore,
    /// else what is installed.
    fn configured_theme_id(&self) -> &str {
        self.theme_preview
            .as_ref()
            .map_or(self.installed_colors.id.as_str(), |preview| {
                preview.restore.as_str()
            })
    }

    /// Collect the palette's Theme section entries from the admitted theme catalog.
    pub(crate) fn palette_themes(&self) -> Vec<PaletteTheme> {
        let current = self.configured_theme_id();
        let mut themes: Vec<PaletteTheme> = aterm_types::scheme::builtin_theme_list()
            .into_iter()
            .map(|builtin| PaletteTheme {
                id: builtin.name.to_string(),
                label: builtin.name.to_string(),
                appearance: builtin.appearance,
                current: builtin.name.eq_ignore_ascii_case(current),
            })
            .collect();
        for (id, meta) in self.config_assets.themes.ready_themes() {
            if aterm_types::scheme::builtin_theme(id).is_some() {
                continue;
            }
            themes.push(PaletteTheme {
                id: id.to_string(),
                label: crate::app_config::theme_file_label(id, meta),
                appearance: meta.appearance,
                current: id.eq_ignore_ascii_case(current),
            });
        }
        themes
    }

    /// Open the command palette scoped to its Theme section.
    pub(crate) fn open_theme_palette(&mut self) {
        if self
            .front()
            .and_then(|ws| ws.palette())
            .is_some_and(PaletteState::is_themes_only)
        {
            return;
        }
        if self.front().and_then(|ws| ws.palette()).is_some() {
            self.palette_exit();
        }
        let Some(wid) = self.frontmost_window else {
            return;
        };
        let state = self.palette_snapshot(wid).themes_only();
        self.palette_open_state(wid, state);
    }

    /// Open the command palette scoped to the front session's tab theme.
    pub(crate) fn open_tab_theme_palette(&mut self) {
        let Some(wid) = self.frontmost_window else {
            return;
        };
        let Some(sid) = self.front_terminal(wid).map(|t| t.session) else {
            return;
        };
        if let Some(previous) = self.palette_tab_theme.as_ref().map(|trial| trial.window) {
            self.palette_exit_in(previous);
            self.sync_tab_theme_trial(false, None);
        }
        if self.front().and_then(|ws| ws.palette()).is_some() {
            self.palette_exit();
        }
        self.palette_tab_theme = Some(crate::session_theme::TabThemeTrial {
            window: wid,
            session: sid,
            prior: self.pool.theme(sid).cloned(),
        });
        let drawn = self.resolved_colors_for_session(sid).id;
        let themes = self
            .palette_themes()
            .into_iter()
            .map(|mut theme| {
                theme.current = theme.id.eq_ignore_ascii_case(&drawn);
                theme
            })
            .collect();
        let mut state = PaletteState::new().with_themes(themes);
        state.resolve(&self.palette_live_for_window(Some(wid)));
        self.palette_open_state(wid, state.themes_only());
    }

    /// Clear the front session's per-tab theme override.
    pub(crate) fn clear_front_tab_theme(&mut self) {
        let Some(sid) = self
            .frontmost_window
            .and_then(|wid| self.front_terminal(wid))
            .map(|t| t.session)
        else {
            return;
        };
        if self.pool.theme(sid).is_some() {
            self.set_session_theme(sid, None);
        }
    }

    /// The Tab-scoped palette's turn: a highlight that moved in the trial's
    /// own window draws its theme on the trial's session only; with that
    /// window's palette closed the trial ends and the session's prior theme
    /// comes back.
    fn sync_tab_theme_trial(&mut self, trial_open: bool, change: Option<Option<String>>) {
        let Some(trial) = self.palette_tab_theme.clone() else {
            return;
        };
        if !trial_open || self.pool.get(trial.session).is_none() {
            self.palette_tab_theme = None;
            if self.pool.get(trial.session).is_some() {
                self.set_session_theme(trial.session, trial.prior);
            }
            return;
        }
        match change {
            Some(Some(id)) if self.theme_preview_unavailable(&id).is_none() => {
                self.set_session_theme(
                    trial.session,
                    Some(crate::session_theme::SessionTheme {
                        selector: id,
                        origin: crate::session_theme::SessionThemeOrigin::Ctl,
                    }),
                );
            }
            Some(_) => self.set_session_theme(trial.session, trial.prior),
            None => {}
        }
    }

    /// Synchronize live theme previews with any open palette's highlighted theme row.
    pub(crate) fn sync_palette_theme_preview(&mut self) {
        let front = self.frontmost_window;
        let trial_window = self.palette_tab_theme.as_ref().map(|trial| trial.window);
        let mut trial_open = false;
        let mut trial_ask: Option<Option<String>> = None;
        let mut any_open = false;
        let mut ask: Option<Option<String>> = None;
        let mut front_ask: Option<Option<String>> = None;
        for (&wid, ws) in &mut self.windows {
            let Some(palette) = ws.palette_mut() else {
                continue;
            };
            let change = palette.take_theme_highlight_change();
            if Some(wid) == trial_window {
                trial_open = true;
                trial_ask = change;
                continue;
            }
            any_open = true;
            if let Some(change) = change {
                if Some(wid) == front {
                    front_ask = Some(change);
                } else {
                    ask = Some(change);
                }
            }
        }
        self.sync_tab_theme_trial(trial_open, trial_ask);
        let palette_held =
            |preview: &crate::control_theme::ThemePreview| preview.hold == PreviewHold::Palette;
        if !any_open {
            self.end_theme_preview_if(palette_held);
            return;
        }
        match front_ask.or(ask) {
            None => {}
            Some(Some(id)) if !id.eq_ignore_ascii_case(self.configured_theme_id()) => {
                if let Err(why) = self.begin_theme_preview(&id, PreviewHold::Palette) {
                    aterm_log::debug!("palette theme preview {id:?}: {why}");
                    self.end_theme_preview_if(palette_held);
                }
            }
            Some(_) => {
                self.end_theme_preview_if(palette_held);
            }
        }
    }

    /// The name the configuration draws on the side the OS appearance picks
    /// now: a plain name, the side of a `dark:…,light:…` pair, or Default
    /// when it names none.
    fn configured_side_name(&self) -> String {
        crate::app_config::resolve_theme_name_value(
            self.config.theme.as_deref(),
            self.os_appearance,
        )
        .unwrap_or_else(|| "Default".to_string())
    }

    /// Commit theme `id` from the palette while holding [`PreviewHold::Committing`].
    pub(crate) fn palette_choose_theme(&mut self, id: &str) {
        // chooses the instance theme as usual.
        if let Some(trial) = self
            .palette_tab_theme
            .take_if(|trial| Some(trial.window) == self.frontmost_window)
        {
            if self.pool.get(trial.session).is_none() {
                return;
            }
            let theme = match self.theme_preview_unavailable(id) {
                None => Some(crate::session_theme::SessionTheme {
                    selector: id.to_string(),
                    origin: crate::session_theme::SessionThemeOrigin::Ctl,
                }),
                Some(_) => trial.prior,
            };
            self.set_session_theme(trial.session, theme);
            return;
        }
        if id.eq_ignore_ascii_case(&self.configured_side_name()) {
            self.end_theme_preview_if(|preview| preview.hold == PreviewHold::Palette);
            return;
        }
        let already = self
            .theme_preview
            .as_mut()
            .filter(|preview| preview.id.eq_ignore_ascii_case(id));
        if let Some(preview) = already {
            preview.hold = PreviewHold::Committing;
        } else if let Err(why) = self.begin_theme_preview(id, PreviewHold::Committing) {
            self.post_message(crate::message_reporters::theme_not_saved(id, &why, false));
            return;
        }
        if let Err(why) = self.queue_theme_choice(id) {
            self.end_theme_preview_if(|preview| preview.hold == PreviewHold::Committing);
            self.post_message(crate::message_reporters::theme_not_saved(id, &why, false));
        }
    }

    /// Compute the front window's live command predicates.
    pub(crate) fn palette_live(&self) -> PaletteLive {
        self.palette_live_for_window(self.frontmost_window)
    }

    /// Resolve content and window facts for the palette's own window.
    fn palette_live_for_window(&self, wid: Option<crate::WindowId>) -> PaletteLive {
        let window = wid.and_then(|wid| self.windows.get(&wid));
        let has_selection = wid.is_some_and(|wid| {
            if self.active_native_view(wid).is_some() {
                self.native_selection_text(wid).is_some()
            } else {
                self.front_terminal(wid).is_some_and(|terminal| {
                    crate::term_lock(&terminal.term)
                        .text_selection()
                        .has_selection()
                })
            }
        });
        let fullscreen = window
            .and_then(|ws| ws.os_window.as_ref())
            .and_then(|w| w.fullscreen())
            .is_some();
        let maximized = window
            .and_then(|ws| ws.os_window.as_ref())
            .is_some_and(|w| w.is_maximized());
        let multi_tab = window.is_some_and(|ws| ws.tab_set.len() >= 2);
        let native_tab_active = wid.is_some_and(|wid| self.active_native_view(wid).is_some());
        let staged = self.relaunch.as_ref().map(|r| (r.build, r.version.clone()));
        let staged_trouble = staged
            .as_ref()
            .and_then(|(build, _)| self.apply_trouble_for(*build));
        let staged_on_hold = staged
            .as_ref()
            .is_some_and(|(build, _)| self.latched_activation_trial_spent(*build));
        let realized = if self.serious_mode_enabled() {
            None
        } else {
            self.upgrade_realized
                .filter(|t| t.elapsed() < crate::relaunch_notice::REALIZED_ARROW_TTL)
                .map(|t| (crate::build_info::version_display().to_string(), t))
        };
        let rain_on = wid
            .and_then(|wid| self.front_terminal(wid))
            .is_some_and(|terminal| self.session_rain_enabled(terminal.session));
        let promotable = wid.map_or(self.launch_kitty, |wid| self.promotable_kitty(wid));
        let kitty_favourited = self.kitty_log.is_favourite(promotable);
        let terminal_front = wid.is_some_and(|wid| self.front_terminal(wid).is_some());
        let can_rename = wid.is_some_and(|wid| self.can_rename_session(wid));
        let front_person_hold = wid.is_some_and(|wid| self.front_person_hold(wid));
        let front_remote_hold = wid.is_some_and(|wid| self.front_remote_hold(wid));
        #[cfg(target_os = "macos")]
        let menu_accels = Vec::new();
        #[cfg(not(target_os = "macos"))]
        let menu_accels = {
            let mut hints: Vec<(crate::menu::MenuAction, String)> = Vec::new();
            for section in crate::menu::MENU_MODEL {
                for (_, action, _, _) in section
                    .entries
                    .iter()
                    .flat_map(crate::menu::MenuEntry::items)
                {
                    if !hints.iter().any(|(a, _)| *a == action)
                        && let Some(binding) = menu_binding(action)
                        && let Some(chord) = self.keybindings.display_chord_for(binding)
                    {
                        hints.push((action, chord));
                    }
                }
            }
            hints
        };
        let tab_theme = wid
            .and_then(|wid| self.front_terminal(wid))
            .is_some_and(|terminal| self.pool.theme(terminal.session).is_some());
        PaletteLive {
            has_selection,
            can_save: wid.is_some_and(|wid| self.can_save_document(wid)),
            can_find: wid.is_some_and(|wid| self.can_find(wid)),
            can_find_again: wid.is_some_and(|wid| self.can_find_again(wid)),
            can_rename,
            tab_theme,
            rain_on,
            kitty_favourited,
            serious_mode: self.serious_mode_enabled(),
            fullscreen,
            maximized,
            front_tab_split: window
                .and_then(|ws| ws.tab_set.active())
                .is_some_and(|tab| tab.root.len() > 1),
            multi_tab,
            native_tab_active,
            terminal_front,
            can_reopen_closed_tab: self.can_reopen_closed_tab(),
            can_reopen_closed_view: self.can_reopen_closed_view(),
            local_file_picker_available: !self.headless
                && crate::menu::local_file_picker_available(),
            front_person_hold,
            front_remote_hold,
            needs_in_windows: self.presence_rim_on(),
            stats_bars: Some(self.stats_bars_form()),
            deck_size: Some(self.deck_size_pref()),
            agents: self.agents_live(),
            staged,
            staged_trouble,
            staged_on_hold,
            realized,
            reduced_motion: self.motion_policy(true) == crate::motion::MotionPolicy::Reduced,
            menu_accels,
        }
    }

    /// Whether the front window's active tab is split, so ⌘W closes a pane
    /// rather than the tab (`close_active_tab`).
    #[cfg(any(target_os = "macos", test))]
    fn front_tab_split(&self) -> bool {
        self.front()
            .and_then(|ws| ws.tab_set.active())
            .is_some_and(|tab| tab.root.len() > 1)
    }

    /// Publish the front window's live menu state ([`crate::menu::MenuLive`]).
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn publish_menu_live(&self) {
        let last = crate::menu::menu_live();
        let front = self.frontmost_window;
        let terminal = front.and_then(|wid| self.front_terminal(wid));
        let has_selection = if front.is_some_and(|wid| self.active_native_view(wid).is_some())
            || self.front_rename_edit().is_some()
        {
            true
        } else if let Some(terminal) = terminal {
            crate::term_try_lock(&terminal.term).map_or(last.has_selection, |term| {
                term.text_selection().has_selection()
            })
        } else {
            false
        };
        let promotable = front.map_or(self.launch_kitty, |wid| self.promotable_kitty(wid));
        crate::menu::set_menu_live(crate::menu::MenuLive {
            has_selection,
            can_save: front.is_some_and(|wid| self.can_save_document(wid)),
            can_find: front.is_some_and(|wid| self.can_find(wid)),
            can_find_again: front.is_some_and(|wid| self.can_find_again(wid)),
            multi_tab: self.front().is_some_and(|ws| ws.tab_set.len() >= 2),
            can_reopen_closed_tab: self.can_reopen_closed_tab(),
            can_reopen_closed_view: self.can_reopen_closed_view(),
            serious_mode: self.serious_mode_enabled(),
            rain_on: terminal.is_some_and(|t| self.session_rain_enabled(t.session)),
            kitty_favourited: self.kitty_log.is_favourite(promotable),
            front_tab_split: self.front_tab_split(),
        });
        crate::menu::set_stats_bars_form(self.stats_bars_form());
        crate::menu::set_deck_size_pref(self.deck_size_pref());
    }

    /// Re-resolve every open command palette and schedule a repaint.
    pub(crate) fn palette_refresh_live(&mut self) {
        let touched: Vec<_> = self
            .windows
            .iter()
            .filter_map(|(&wid, ws)| ws.palette().is_some().then_some(wid))
            .collect();
        for &wid in &touched {
            let live = self.palette_live_for_window(Some(wid));
            if let Some(ws) = self.windows.get_mut(&wid)
                && let Some(p) = ws.palette_mut()
            {
                p.resolve(&live);
                if let Some(w) = &ws.os_window {
                    w.request_redraw();
                }
            }
        }
        for wid in touched.iter().copied() {
            self.sync_palette_pointer_cursor(wid);
        }
        if !touched.is_empty() {
            // The row set changed under a screen reader too.
            self.overlay_a11y_update();
        }
    }

    /// Re-scope an open command palette on `wid` after the active tab changes.
    pub(crate) fn palette_sync_native_scope(&mut self, wid: crate::WindowId) {
        let scope = self.native_palette_scope(wid);
        let live = self.palette_live_for_window(Some(wid));
        let mut changed = false;
        if let Some(palette) = self
            .windows
            .get_mut(&wid)
            .and_then(|window| window.palette_mut())
        {
            palette.replace_native_commands(scope);
            palette.resolve(&live);
            changed = true;
        }
        if !changed {
            return;
        }
        self.sync_palette_pointer_cursor(wid);
        self.invalidate_native_ui_cache(wid);
        if let Some(window) = self
            .windows
            .get(&wid)
            .and_then(|window| window.os_window.as_ref())
        {
            window.request_redraw();
        }
        self.overlay_a11y_update();
    }

    /// Open the command palette on the front window.
    pub(crate) fn palette_enter(&mut self) {
        let Some(wid) = self.frontmost_window else {
            return;
        };
        let state = self.palette_snapshot(wid);
        self.palette_open_state(wid, state);
    }

    /// Put `state` up in its owning window (no-op if one is open).
    fn palette_open_state(&mut self, wid: crate::WindowId, state: PaletteState) {
        let mut opened = false;
        if let Some(ws) = self.windows.get_mut(&wid)
            && ws.palette().is_none()
        {
            ws.overlay = Some(crate::overlay::Overlay::Palette(state));
            ws.scroll_residual = 0.0;
            ws.scroll_residual_x = 0.0;
            if let Some(w) = &ws.os_window {
                w.request_redraw();
            }
            opened = true;
        }
        if opened {
            self.settle_pointer_drags(wid);
            let _ = self.palette_claims_pointer(wid);
            if let Some((x, y)) = self.windows.get(&wid).map(|window| window.last_cursor_px) {
                self.palette_pointer_motion(wid, x, y);
            }
        }
        self.overlay_a11y_update();
    }

    /// Open the command palette scoped to recovery actions for a blocked native close.
    pub(crate) fn palette_enter_native_close_recovery(
        &mut self,
        wid: crate::WindowId,
        instance: crate::tab_model::AppInstanceId,
        view: crate::tab_model::ViewId,
        commands: Vec<crate::native_app::Command>,
    ) -> Result<(), String> {
        if commands.is_empty() {
            return Err("native close was blocked without a recovery command".to_string());
        }
        let generation = self
            .native_runtime
            .view_generation(view)
            .ok_or_else(|| "blocked native view no longer has a generation".to_string())?;
        let app = self
            .native_runtime
            .app(instance)
            .ok_or_else(|| "blocked native app no longer exists".to_string())?;
        let mut state = PaletteState::native_close_recovery(NativeCommandScope {
            window: wid,
            instance,
            view,
            generation,
            section: format!("{} Close Recovery", app.descriptor().name),
            commands,
        });
        state.resolve(&self.palette_live_for_window(Some(wid)));

        let window = self
            .windows
            .get_mut(&wid)
            .ok_or_else(|| "blocked native close window disappeared".to_string())?;
        window.overlay = Some(crate::overlay::Overlay::Palette(state));
        window.scroll_residual = 0.0;
        window.scroll_residual_x = 0.0;
        if let Some(os_window) = &window.os_window {
            os_window.request_redraw();
        }
        self.settle_pointer_drags(wid);
        let _ = self.palette_claims_pointer(wid);
        self.invalidate_native_ui_cache(wid);
        self.overlay_a11y_update();
        Ok(())
    }

    /// Close the command palette on the front window.
    pub(crate) fn palette_exit(&mut self) {
        if let Some(wid) = self.frontmost_window {
            self.palette_exit_in(wid);
        }
    }

    /// Close a palette by its actual window, without moving front-window custody.
    fn palette_exit_in(&mut self, wid: crate::WindowId) {
        let mut closed = false;
        if let Some(ws) = self.windows.get_mut(&wid)
            && ws.palette().is_some()
        {
            ws.overlay = None;
            ws.scroll_residual = 0.0;
            ws.scroll_residual_x = 0.0;
            if let Some(w) = &ws.os_window {
                w.request_redraw();
            }
            closed = true;
        }
        #[cfg(a11y_tree)]
        self.push_a11y_tree(wid);
        if closed {
            self.end_theme_preview_if(|preview| preview.hold == PreviewHold::Palette);
        }
        if closed {
            // a pointer the window no longer holds is not replayed.
            self.replay_held_pointer(wid);
        }
    }

    /// Toggle the command palette on the front window.
    pub(crate) fn toggle_palette(&mut self) {
        if self.front().is_some_and(|ws| ws.palette().is_some()) {
            self.palette_exit();
        } else {
            self.palette_enter();
        }
    }

    /// Move the palette selection cursor by `delta` over the filtered rows.
    pub(crate) fn palette_move(&mut self, delta: isize) {
        if let Some(p) = self.front_mut().and_then(|ws| ws.palette_mut()) {
            p.move_selection(delta);
        }
        self.palette_repaint_front();
    }

    /// Append filter character `c` and re-filter the open palette.
    #[cfg(test)]
    pub(crate) fn palette_filter_push(&mut self, c: char) {
        if let Some(p) = self.front_mut().and_then(|ws| ws.palette_mut()) {
            p.push_char(c);
        }
        self.palette_repaint_front();
    }

    /// Activate the currently selected palette target.
    pub(crate) fn palette_activate(&mut self) {
        let Some(target) = self
            .front()
            .and_then(|ws| ws.palette())
            .and_then(PaletteState::selected_target)
        else {
            return;
        };
        if let PaletteTarget::Theme(id) = target {
            self.palette_choose_theme(&id);
            self.palette_exit();
            return;
        }
        self.palette_exit();
        match target {
            PaletteTarget::Menu(crate::menu::MenuAction::TakeOver) => {
                if let Some(wid) = self.frontmost_window {
                    self.take_over(wid);
                }
            }
            PaletteTarget::Menu(crate::menu::MenuAction::HandBack) => {
                if let Some(wid) = self.frontmost_window {
                    self.hand_back(wid);
                }
            }
            PaletteTarget::Menu(action) => {
                self.post_menu_action(action);
            }
            PaletteTarget::Native(target) => {
                let _ = self.activate_native_palette_target(&target);
            }
            PaletteTarget::Theme(_) => {}
        }
    }

    /// Dispatch a native palette command if its window, view, generation, and enabled state still hold.
    pub(crate) fn activate_native_palette_target(
        &mut self,
        target: &NativeCommandTarget,
    ) -> Result<(), String> {
        if self.active_native_view(target.window) != Some((target.instance, target.view)) {
            return Err("native palette target is no longer active".to_string());
        }
        if self.native_runtime.view_generation(target.view) != Some(target.generation) {
            return Err("native palette target generation is stale".to_string());
        }
        let enabled = self
            .native_runtime
            .commands(target.instance, target.view)
            .map_err(|error| format!("native command refresh failed: {error:?}"))?
            .into_iter()
            .find(|command| command.id == target.action)
            .is_some_and(|command| command.enabled);
        if !enabled {
            return Err("native palette command is disabled or no longer exists".to_string());
        }
        self.dispatch_native_view_event(
            target.window,
            target.view,
            crate::native_app::AppEvent::Action(crate::native_app::ActionInvocation {
                id: target.action.clone(),
                value: None,
            }),
        )?;
        Ok(())
    }

    fn palette_repaint_front(&mut self) {
        if let Some(wid) = self.frontmost_window {
            self.sync_palette_pointer_cursor(wid);
        }
        if let Some(ws) = self.front_mut()
            && let Some(w) = &ws.os_window
        {
            w.request_redraw();
        }
        self.overlay_a11y_update();
    }

    /// Apply a [`SearchEdit`](crate::app_search::SearchEdit) to the open palette filter.
    pub(crate) fn palette_edit(&mut self, edit: crate::app_search::SearchEdit) {
        if let Some(p) = self.front_mut().and_then(|ws| ws.palette_mut()) {
            p.edit(edit);
        }
        self.palette_repaint_front();
    }

    /// Execute a classified [`PaletteKey`] on the open palette.
    pub(crate) fn apply_palette_key(&mut self, key: PaletteKey) {
        match key {
            PaletteKey::Close => self.palette_exit(),
            PaletteKey::Move(delta) => self.palette_move(delta),
            PaletteKey::Activate => self.palette_activate(),
            PaletteKey::Edit(edit) => self.palette_edit(edit),
            PaletteKey::Inert => {}
        }
    }

    /// Handle a physical key press while the palette is open on `wid`.
    pub(crate) fn on_key_palette_mode(
        &mut self,
        wid: crate::WindowId,
        mods: aterm_window::keyboard::ModifiersState,
        ev: &aterm_window::event::KeyEvent,
    ) -> bool {
        if self.windows.get(&wid).and_then(|ws| ws.palette()).is_none() {
            return false;
        }
        self.apply_palette_key(palette_key(mods, ev));
        true
    }

    /// Handle an engine-neutral [`InputEvent`](crate::input::InputEvent) while the palette is open on `wid`.
    pub(crate) fn palette_input_event(
        &mut self,
        wid: crate::WindowId,
        ev: &crate::input::InputEvent,
    ) {
        if self.windows.get(&wid).and_then(|ws| ws.palette()).is_none() {
            return;
        }
        let folded = ev.keypad_folded();
        let ev = folded.as_ref().unwrap_or(ev);
        if let Some(key) = palette_input_key(ev) {
            self.apply_palette_key(key);
        }
    }
}

/// Action produced by classifying a keystroke aimed at an open command palette.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PaletteKey {
    /// Close the palette (`Escape`).
    Close,
    /// Move the cursor by `delta` (`Up`/`Down`).
    Move(isize),
    /// Activate the selected row (`Enter`).
    Activate,
    /// Edit the filter text ([`crate::palette::PaletteState::edit`]).
    Edit(crate::app_search::SearchEdit),
    /// Swallow the keystroke with no effect.
    Inert,
}

/// Classify a winit keystroke aimed at the open command palette.
pub(crate) fn palette_key(
    mods: aterm_window::keyboard::ModifiersState,
    ev: &aterm_window::event::KeyEvent,
) -> PaletteKey {
    use crate::app_search::SearchEdit;
    use aterm_window::keyboard::{Key, NamedKey};
    match &ev.logical_key {
        Key::Named(NamedKey::Escape) => return PaletteKey::Close,
        Key::Named(NamedKey::ArrowUp) => return PaletteKey::Move(-1),
        Key::Named(NamedKey::ArrowDown) => return PaletteKey::Move(1),
        Key::Named(NamedKey::Enter) => return PaletteKey::Activate,
        _ => {}
    }
    if let Some(edit) = crate::app_input::field_edit_action(mods, ev) {
        return PaletteKey::Edit(edit);
    }
    if mods.control_key() && !mods.alt_key() && matches!(ev.logical_key, Key::Character(_)) {
        return PaletteKey::Inert;
    }
    let glyphs: String = match &ev.logical_key {
        Key::Named(NamedKey::Space) => " ".to_string(),
        Key::Character(s) => s.to_string(),
        _ => ev.text.as_deref().unwrap_or_default().to_string(),
    };
    let glyphs: String = glyphs.chars().filter(|c| !c.is_control()).collect();
    if glyphs.is_empty() {
        PaletteKey::Inert
    } else {
        PaletteKey::Edit(SearchEdit::Insert(glyphs))
    }
}

/// Classify an engine-neutral [`InputEvent`](crate::input::InputEvent) aimed at the open command palette.
pub(crate) fn palette_input_key(ev: &crate::input::InputEvent) -> Option<PaletteKey> {
    use crate::app_search::SearchEdit;
    use crate::input::InputEvent;
    use aterm_types::keyboard::{Key as TKey, KeyEventType, Modifiers, NamedKey as TNamed};
    let (key, mods, layout_text) = match ev {
        InputEvent::Key {
            event_type: KeyEventType::Release,
            ..
        } => return None,
        InputEvent::Key {
            key,
            mods,
            layout_text,
            ..
        } => (key, *mods, *layout_text),
        InputEvent::Text(text) | InputEvent::Paste(text, _) => {
            return Some(PaletteKey::Edit(SearchEdit::Insert(text.clone())));
        }
        _ => return None,
    };
    match key {
        TKey::Named(TNamed::Escape) => return Some(PaletteKey::Close),
        TKey::Named(TNamed::ArrowUp) => return Some(PaletteKey::Move(-1)),
        TKey::Named(TNamed::ArrowDown) => return Some(PaletteKey::Move(1)),
        TKey::Named(TNamed::Enter) => return Some(PaletteKey::Activate),
        _ => {}
    }
    if let Some(edit) = crate::app_input::field_edit_action_engine(key, mods, layout_text) {
        return Some(PaletteKey::Edit(edit));
    }
    let command = mods.contains(Modifiers::SUPER)
        || (mods.contains(Modifiers::CTRL) && !mods.contains(Modifiers::ALT));
    let glyph = match key {
        TKey::Character(_) if command => return Some(PaletteKey::Inert),
        TKey::Character(c) => crate::input::typed_glyph(*c, mods, layout_text),
        TKey::Named(TNamed::Space) => ' ',
        _ => return Some(PaletteKey::Inert),
    };
    Some(if glyph.is_control() {
        PaletteKey::Inert
    } else {
        PaletteKey::Edit(SearchEdit::Insert(glyph.to_string()))
    })
}

#[cfg(test)]
mod tests {
    use crate::App;
    use crate::WindowId;

    fn filter(app: &mut App, query: &str) {
        for c in query.chars() {
            app.palette_filter_push(c);
        }
    }

    #[test]
    fn one_shared_find_row_opens_the_active_native_search() {
        use crate::menu::MenuAction;
        use crate::native_app::{AppEvent, AppKind, AppViewState};
        use crate::native_editor::Minibuffer;
        use crate::palette::PaletteTarget;

        fn choose_find(app: &mut App, wid: WindowId) {
            let snapshot = app.palette_snapshot(wid);
            let rows: Vec<_> = snapshot
                .rows()
                .iter()
                .filter(|row| match &row.action {
                    PaletteTarget::Menu(MenuAction::Find) => true,
                    PaletteTarget::Native(target) => {
                        matches!(target.action.as_str(), "editor/find" | "settings/search")
                    }
                    _ => false,
                })
                .collect();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].action, MenuAction::Find);
            assert!(rows[0].enabled);
            app.palette_enter();
            filter(app, "find");
            let chosen = app.windows[&wid]
                .palette()
                .unwrap()
                .selected_action()
                .expect("selected shared Find action");
            assert_eq!(chosen, MenuAction::Find);
            app.palette_activate();
            assert!(app.windows[&wid].palette().is_none());
            app.dispatch_action(wid, crate::keybinding::Action::Find);
        }

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        choose_find(&mut app, wid);
        let (instance, view) = app.active_native_view(wid).unwrap();
        assert!(matches!(
            app.native_runtime.view_state(view),
            Some(AppViewState::Settings(state)) if state.common.last_focus.as_ref()
                .is_some_and(|key| key.as_str() == "settings/search")
        ));
        assert!(
            app.native_runtime
                .commands(instance, view)
                .unwrap()
                .iter()
                .any(|command| command.id.as_str() == "settings/search")
        );
        assert!(app.windows[&wid].search.is_none());

        let scratch = aterm_tempfile::TempDir::new().unwrap();
        let path = scratch.path().join("find.txt");
        let source = "one needle two needle";
        std::fs::write(&path, source).unwrap();
        let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
        app.open_document_tab(AppKind::Editor, &uri).unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        let original = app.document_store.snapshot(document).unwrap();
        choose_find(&mut app, wid);
        assert!(matches!(
            app.native_runtime.view_state(view),
            Some(AppViewState::Editor(state)) if state.buffer.as_ref()
                .is_some_and(|buffer| matches!(buffer.minibuffer, Minibuffer::Search { .. }))
        ));
        assert!(
            app.native_runtime
                .commands(instance, view)
                .unwrap()
                .iter()
                .any(|command| command.id.as_str() == "editor/find")
        );
        app.dispatch_native_event(
            wid,
            AppEvent::EditorInput(amacs::input::EditorInput::Text("needle".to_string())),
        )
        .unwrap();
        let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
            panic!("editor view");
        };
        let at = source.find("needle").unwrap();
        assert_eq!(
            state.buffer.as_ref().unwrap().primary_selection().range(),
            at..at + "needle".len()
        );
        let after = app.document_store.snapshot(document).unwrap();
        assert_eq!(after.seq, original.seq);
        assert_eq!(after.text, original.text);
        assert!(app.windows[&wid].search.is_none());
    }

    #[test]
    fn each_window_palette_keeps_its_own_find_availability_on_snapshot_rescope_and_refresh() {
        use crate::menu::MenuAction;
        use crate::native_app::{AppEvent, AppKind};
        use crate::native_editor::EditorCommand;
        use crate::palette::PaletteState;
        use amacs::input::EditorInput;
        use aterm_types::keyboard::{Key, Modifiers, NamedKey};

        fn find_enabled(palette: &PaletteState) -> [bool; 3] {
            [MenuAction::Find, MenuAction::FindNext, MenuAction::FindPrev].map(|action| {
                palette
                    .rows()
                    .iter()
                    .find(|row| row.action == action)
                    .expect("shared Find row")
                    .enabled
            })
        }

        let scratch = aterm_tempfile::TempDir::new().unwrap();
        let path = scratch.path().join("find.md");
        std::fs::write(&path, "needle then needle\n").unwrap();
        let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
        let mut app = App::headless_for_test();
        let editor_window = WindowId(0);
        app.open_document_tab_in_window(editor_window, AppKind::Editor, &uri)
            .unwrap();
        let (instance, _) = app.active_native_view(editor_window).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        let original = app.document_store.snapshot(document).unwrap();
        app.dispatch_native_event(
            editor_window,
            AppEvent::EditorCommand(EditorCommand::IncrementalSearch),
        )
        .unwrap();
        app.dispatch_native_event(
            editor_window,
            AppEvent::EditorInput(EditorInput::Text("needle".to_string())),
        )
        .unwrap();
        app.dispatch_native_event(
            editor_window,
            AppEvent::EditorInput(EditorInput::Key {
                key: Key::Named(NamedKey::Enter),
                mods: Modifiers::empty(),
                text: None,
            }),
        )
        .unwrap();

        let sid = app.next_session_id;
        let reader_window = app.insert_logical_window(crate::stub_session(sid), 24, 80);
        app.open_document_tab_in_window(reader_window, AppKind::Markdown, &uri)
            .unwrap();
        assert_eq!(app.frontmost_window, Some(reader_window));
        let editor_palette = app.palette_snapshot(editor_window);
        let reader_palette = app.palette_snapshot(reader_window);
        assert_eq!(find_enabled(&editor_palette), [true; 3]);
        assert_eq!(find_enabled(&reader_palette), [false; 3]);
        assert!(
            !app.palette_live().can_find,
            "the menu still follows the front window"
        );

        app.palette_open_state(editor_window, editor_palette);
        assert!(app.windows[&editor_window].palette().is_some());
        assert!(app.windows[&reader_window].palette().is_none());
        app.palette_open_state(reader_window, reader_palette);
        app.palette_refresh_live();
        assert_eq!(
            find_enabled(app.windows[&editor_window].palette().unwrap()),
            [true; 3]
        );
        assert_eq!(
            find_enabled(app.windows[&reader_window].palette().unwrap()),
            [false; 3]
        );

        app.palette_sync_native_scope(editor_window);
        assert_eq!(
            find_enabled(app.windows[&editor_window].palette().unwrap()),
            [true; 3]
        );
        assert_eq!(app.frontmost_window, Some(reader_window));
        let after = app.document_store.snapshot(document).unwrap();
        assert_eq!(after.seq, original.seq);
        assert_eq!(after.text, original.text);
    }

    fn assert_find_availability(app: &App, wid: WindowId, open: bool, again: bool) {
        use crate::menu::{MenuAction, menu_live};

        app.publish_menu_live();
        let native = menu_live();
        let live = app.palette_live();
        assert_eq!((native.can_find, native.can_find_again), (open, again));
        assert_eq!((live.can_find, live.can_find_again), (open, again));
        let palette = app.palette_snapshot(wid);
        for (action, enabled) in [
            (MenuAction::Find, open),
            (MenuAction::FindNext, again),
            (MenuAction::FindPrev, again),
        ] {
            let row = palette
                .rows()
                .iter()
                .find(|row| row.action == action)
                .expect("Find command row");
            assert_eq!(row.enabled, enabled, "{action:?}");
            assert_eq!(
                palette.action_by_name(&format!("{action:?}")).is_ok(),
                enabled,
                "{action:?}: disabled rows cannot activate"
            );
        }
    }

    #[test]
    fn editor_find_availability_preserves_other_prompts_and_restores_after_exit() {
        use crate::native_app::{AppEvent, AppKind, AppViewState};
        use crate::native_editor::{EditorBufferView, EditorCommand, Minibuffer};
        use crate::native_ui::UiKey;
        use amacs::input::EditorInput;
        use aterm_types::keyboard::{Key, Modifiers, NamedKey};

        fn buffer(app: &App, view: crate::tab_model::ViewId) -> &EditorBufferView {
            match app.native_runtime.view_state(view) {
                Some(AppViewState::Editor(state)) => state.buffer.as_ref().unwrap(),
                _ => panic!("editor view"),
            }
        }

        fn assert_editor_find(app: &App, wid: WindowId, open: bool, again: bool) {
            assert_find_availability(app, wid, open, again);
            let (instance, view) = app.active_native_view(wid).unwrap();
            let commands = app.native_runtime.commands(instance, view).unwrap();
            let find = commands
                .iter()
                .find(|command| command.id.as_str() == "editor/find")
                .unwrap();
            assert_eq!(find.enabled, open);
            let compiled = app.compiled_native_ui(wid).unwrap();
            let toolbar = compiled
                .semantic(&UiKey::new("editor/find-button"))
                .expect("editor Find toolbar button");
            assert_eq!(toolbar.state.unwrap().enabled, open);
        }

        let scratch = aterm_tempfile::TempDir::new().unwrap();
        let path = scratch.path().join("prompts.txt");
        let source = "needle one\nneedle two\n";
        std::fs::write(&path, source).unwrap();
        let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.open_document_tab(AppKind::Editor, &uri).unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        let original = app.document_store.snapshot(document).unwrap();
        let enter = AppEvent::EditorInput(EditorInput::Key {
            key: Key::Named(NamedKey::Enter),
            mods: Modifiers::empty(),
            text: None,
        });

        assert_editor_find(&app, wid, true, false);
        app.find_requested_in(wid);
        app.dispatch_native_event(
            wid,
            AppEvent::EditorInput(EditorInput::Text("needle".into())),
        )
        .unwrap();
        app.dispatch_native_event(wid, enter.clone()).unwrap();
        assert_editor_find(&app, wid, true, true);

        for command in [
            EditorCommand::GotoLine,
            EditorCommand::ExecuteCommand,
            EditorCommand::SwitchBuffer,
            EditorCommand::KillBuffer,
        ] {
            app.dispatch_native_event(wid, AppEvent::EditorCommand(command))
                .unwrap();
            app.dispatch_native_event(wid, AppEvent::EditorInput(EditorInput::Text("2".into())))
                .unwrap();
            let prompt = buffer(&app, view).minibuffer.clone();
            assert!(matches!(
                (command, &prompt),
                (EditorCommand::GotoLine, Minibuffer::GotoLine { .. })
                    | (EditorCommand::ExecuteCommand, Minibuffer::Command { .. })
                    | (EditorCommand::SwitchBuffer, Minibuffer::Buffer { .. })
                    | (EditorCommand::KillBuffer, Minibuffer::Prompt { .. })
            ));
            assert_editor_find(&app, wid, false, false);
            app.find_requested_in(wid);
            assert_eq!(buffer(&app, view).minibuffer, prompt);
            assert_editor_find(&app, wid, false, false);
            let exit = if command == EditorCommand::GotoLine {
                enter.clone()
            } else {
                AppEvent::EditorCommand(EditorCommand::Abort)
            };
            app.dispatch_native_event(wid, exit).unwrap();
            assert!(matches!(
                buffer(&app, view).minibuffer,
                Minibuffer::Inactive
            ));
            assert_editor_find(&app, wid, true, true);
        }

        app.find_requested_in(wid);
        app.dispatch_native_event(
            wid,
            AppEvent::EditorInput(EditorInput::Text("needle".into())),
        )
        .unwrap();
        assert_editor_find(&app, wid, true, true);
        let first = buffer(&app, view).primary_selection().range();
        app.find_requested_in(wid);
        assert_ne!(buffer(&app, view).primary_selection().range(), first);
        assert!(matches!(
            &buffer(&app, view).minibuffer,
            Minibuffer::Search { query, .. } if query == "needle"
        ));
        app.search_find_again_in(wid, false).unwrap();
        assert_eq!(buffer(&app, view).primary_selection().range(), first);
        app.dispatch_native_event(wid, enter).unwrap();
        assert_editor_find(&app, wid, true, true);

        let after = app.document_store.snapshot(document).unwrap();
        assert_eq!(after.seq, original.seq);
        assert_eq!(after.text, original.text);
        assert_eq!(std::fs::read_to_string(path).unwrap(), source);
    }

    /// Find availability follows the focused content and its own query through
    /// editing, acceptance and tab switches.
    #[test]
    fn find_availability_tracks_content_and_query_without_changing_documents() {
        use crate::app_search::SearchEdit;
        use crate::native_app::{AppEvent, AppKind, AppViewState};
        use amacs::input::EditorInput;
        use aterm_types::keyboard::{Key, Modifiers, NamedKey};

        let scratch = aterm_tempfile::Builder::new()
            .prefix("aterm-find-availability")
            .tempdir()
            .unwrap();
        let first = scratch.path().join("first.md");
        let second = scratch.path().join("second.txt");
        std::fs::write(&first, "needle then needle\n").unwrap();
        std::fs::write(&second, "another needle\n").unwrap();
        let uri =
            |path: &std::path::Path| crate::native_document_host::path_to_file_uri(path).unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert_find_availability(&app, wid, true, false);
        app.search_last_query = "remembered".to_string();
        assert_find_availability(&app, wid, true, true);
        app.search_enter();
        assert_find_availability(&app, wid, true, false);
        app.search_edit_in(wid, SearchEdit::Insert("needle".to_string()));
        assert_find_availability(&app, wid, true, true);
        app.search_accept_in(wid);
        assert_find_availability(&app, wid, true, true);

        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        assert_find_availability(&app, wid, true, false);
        app.find_requested();
        let (_, settings) = app.active_native_view(wid).unwrap();
        assert_eq!(
            app.native_runtime
                .view_state(settings)
                .unwrap()
                .common()
                .last_focus
                .as_ref()
                .map(crate::native_ui::UiKey::as_str),
            Some("settings/search")
        );
        assert!(app.search_find_again(true).is_err());

        app.open_document_tab(AppKind::Markdown, &uri(&first))
            .unwrap();
        assert_find_availability(&app, wid, false, false);
        assert!(app.search_find_again(false).is_err());

        app.open_document_tab(AppKind::Editor, &uri(&first))
            .unwrap();
        let (instance, _) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        let original = app.document_store.snapshot(document).unwrap();
        assert_find_availability(&app, wid, true, false);
        assert!(app.search_find_again(true).is_err());
        app.find_requested();
        app.dispatch_native_event(
            wid,
            AppEvent::EditorInput(EditorInput::Text("needle".to_string())),
        )
        .unwrap();
        assert_find_availability(&app, wid, true, true);
        app.search_find_again(true).unwrap();
        app.dispatch_native_event(
            wid,
            AppEvent::EditorInput(EditorInput::Key {
                key: Key::Named(NamedKey::Enter),
                mods: Modifiers::empty(),
                text: None,
            }),
        )
        .unwrap();
        assert_find_availability(&app, wid, true, true);
        app.search_find_again(false).unwrap();
        let after = app.document_store.snapshot(document).unwrap();
        assert_eq!(after.seq, original.seq);
        assert_eq!(after.text, original.text);

        app.open_document_tab(AppKind::Editor, &uri(&second))
            .unwrap();
        assert_find_availability(&app, wid, true, false);
        let (_, view) = app.active_native_view(wid).unwrap();
        let Some(AppViewState::Editor(state)) = app.native_runtime.view_state_mut(view) else {
            panic!("editor view");
        };
        state.buffer = None;
        assert_find_availability(&app, wid, false, false);
        app.frontmost_window = None;
        assert_find_availability(&app, wid, false, false);
    }

    /// `publish_menu_live` (what `validateMenuItem:` reads) derives each bit from the same fact
    /// as `palette_live`: one tab greys the tab cycle, no selection greys Copy, serious mode
    /// checks its row, and a split front tab titles ⌘W's row Close Pane.
    #[test]
    fn the_menu_bar_publishes_what_the_palette_resolves() {
        use crate::menu::{MenuAction, menu_live, native_live_title, set_menu_live};
        let before = menu_live();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.frontmost_window = Some(wid);
        let agrees = |app: &App| {
            app.publish_menu_live();
            let bar = menu_live();
            let palette = app.palette_live();
            assert_eq!(bar.has_selection, palette.has_selection);
            assert_eq!(bar.can_save, palette.can_save);
            assert_eq!(bar.can_find, palette.can_find);
            assert_eq!(bar.can_find_again, palette.can_find_again);
            assert_eq!(bar.multi_tab, palette.multi_tab);
            assert_eq!(bar.can_reopen_closed_tab, palette.can_reopen_closed_tab);
            assert_eq!(bar.can_reopen_closed_view, palette.can_reopen_closed_view);
            assert_eq!(bar.serious_mode, palette.serious_mode);
            assert_eq!(bar.rain_on, palette.rain_on);
            assert_eq!(bar.kitty_favourited, palette.kitty_favourited);
            assert_eq!(bar.front_tab_split, palette.front_tab_split);
            bar
        };
        let one = agrees(&app);
        assert!(!one.multi_tab && !one.has_selection && !one.front_tab_split);
        assert_eq!(native_live_title(MenuAction::CloseTab), Some("Close Tab"));

        app.set_serious_mode(!one.serious_mode);
        assert_eq!(agrees(&app).serious_mode, !one.serious_mode);

        app.split_active_stub_tab(wid);
        assert!(agrees(&app).front_tab_split);
        assert_eq!(native_live_title(MenuAction::CloseTab), Some("Close Pane"));

        app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
        let two = agrees(&app);
        assert!(
            two.multi_tab && !two.front_tab_split,
            "a fresh one-pane tab is front"
        );
        set_menu_live(before);
    }

    /// An open rename keeps the menu bar's Copy row enabled so ⌘C reaches the rename divert.
    #[test]
    fn a_live_rename_keeps_the_menu_bar_copy_row_enabled() {
        use crate::menu::{menu_live, set_menu_live};
        let before = menu_live();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.frontmost_window = Some(wid);
        app.tab_strip_rows = 1;
        app.publish_menu_live();
        assert!(!menu_live().has_selection, "no selection, no edit: greyed");

        let tab = app.windows[&wid].tab_set.active_id().expect("a tab");
        assert!(app.begin_session_rename(wid, tab));
        app.publish_menu_live();
        assert!(menu_live().has_selection, "an open rename field keeps Copy");

        let session = app.rename_edit_session(wid).expect("editing");
        app.cancel_session_rename(wid, session);
        app.publish_menu_live();
        assert!(!menu_live().has_selection, "the edit closed: greyed again");
        set_menu_live(before);
    }

    /// A ⌘ chord is a command, never text: the key equivalents of rows the bar greys (⌘C with
    /// no selection, ⇧⌘] with one tab, ⇧⌘T with nothing to reopen) reach `on_key` and type
    /// nothing into an open palette's search.
    #[test]
    fn a_command_chord_never_types_into_the_palette_search() {
        use aterm_window::event::{ElementState, KeyEvent};
        use aterm_window::keyboard::{
            Key, KeyCode, KeyLocation, ModifiersState, PhysicalKey, SmolStr,
        };
        let press = |code, text: &str| {
            KeyEvent::synthetic_for_test(
                PhysicalKey::Code(code),
                Key::Character(SmolStr::new(text)),
                Some(SmolStr::new(text)),
                KeyLocation::Standard,
                ElementState::Pressed,
                false,
            )
        };
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.frontmost_window = Some(wid);
        app.palette_enter();
        let query = |app: &App| {
            app.windows[&wid]
                .palette()
                .expect("the palette stays open")
                .controls_lines()[0]
                .clone()
        };
        app.windows.get_mut(&wid).unwrap().mods = ModifiersState::SUPER;
        app.on_key(wid, press(KeyCode::KeyC, "c"));
        app.windows.get_mut(&wid).unwrap().mods = ModifiersState::SUPER | ModifiersState::SHIFT;
        app.on_key(wid, press(KeyCode::BracketRight, "}"));
        app.on_key(wid, press(KeyCode::KeyT, "T"));
        assert!(query(&app).contains("query=\"\""), "{}", query(&app));
        app.windows.get_mut(&wid).unwrap().mods = ModifiersState::empty();
        app.on_key(wid, press(KeyCode::KeyC, "c"));
        assert!(query(&app).contains("query=\"c\""), "{}", query(&app));
    }

    #[test]
    fn toggle_opens_and_closes_palette() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.windows.get(&wid).unwrap().palette().is_none());
        app.toggle_palette();
        assert!(app.windows.get(&wid).unwrap().palette().is_some(), "opened");
        app.toggle_palette();
        assert!(app.windows.get(&wid).unwrap().palette().is_none(), "closed");
    }

    /// `find_next`/`find_prev` ship unseeded (F3 belongs to the program behind the PTY), so
    /// their rows are blank until a user binds them, then show the chord.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn find_again_rows_show_a_user_bound_chord() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let accel_of = |app: &App, label: &str| {
            app.palette_snapshot(wid)
                .controls_lines()
                .into_iter()
                .find(|line| line.contains(&format!("label={label:?}")))
                .unwrap_or_else(|| panic!("{label} row"))
        };
        app.keybindings = crate::keybinding::Keybindings::resolved(None);
        assert!(
            accel_of(&app, "Find Next").contains(r#"accel="""#),
            "unseeded by default — blank, never a guess: {}",
            accel_of(&app, "Find Next"),
        );
        let mut table = std::collections::BTreeMap::new();
        table.insert("f3".to_string(), "find_next".to_string());
        table.insert("shift+f3".to_string(), "find_prev".to_string());
        app.keybindings = crate::keybinding::Keybindings::resolved(Some(&table));
        assert!(
            accel_of(&app, "Find Next").contains(r#"accel="F3""#),
            "{}",
            accel_of(&app, "Find Next"),
        );
        assert!(
            accel_of(&app, "Find Previous").contains(r#"accel="Shift+F3""#),
            "{}",
            accel_of(&app, "Find Previous"),
        );
    }

    #[test]
    fn serious_mode_hides_realized_upgrade_row_and_restores_live_underlying_state() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.upgrade_realized = Some(std::time::Instant::now());
        app.palette_enter();
        let has_realized_row = |app: &App| {
            app.windows[&wid]
                .palette()
                .expect("open palette")
                .controls_lines()
                .iter()
                .any(|line| line.contains("Updated to aterm v"))
        };
        assert!(
            has_realized_row(&app),
            "negative control: celebration is live"
        );

        assert!(app.set_serious_mode(true));
        assert!(app.palette_live().realized.is_none());
        assert!(
            !has_realized_row(&app),
            "the retained open palette re-resolves immediately"
        );

        assert!(!app.set_serious_mode(false));
        assert!(app.palette_live().realized.is_some());
        assert!(
            has_realized_row(&app),
            "disabling the override restores an unexpired underlying celebration"
        );
    }

    #[test]
    fn filter_then_activate_posts_the_action() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.palette_enter();
        for c in "help".chars() {
            app.palette_filter_push(c);
        }
        let action = app
            .windows
            .get(&wid)
            .unwrap()
            .palette()
            .unwrap()
            .selected_action();
        assert_eq!(action, Some(crate::menu::MenuAction::Help));
        app.palette_activate();
        assert!(
            app.windows.get(&wid).unwrap().palette().is_none(),
            "activate closes"
        );
    }

    #[test]
    fn active_native_commands_are_scoped_and_dispatch_the_exact_action() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (instance, view) = app.active_native_view(wid).expect("Settings active");
        let generation = app
            .native_runtime
            .view_generation(view)
            .expect("live Settings generation");

        app.palette_enter();
        filter(&mut app, "Settings: Go to Themes");
        let target = app
            .windows
            .get(&wid)
            .and_then(|window| window.palette())
            .and_then(crate::palette::PaletteState::selected_target)
            .expect("native route command");
        assert!(matches!(
            &target,
            crate::palette::PaletteTarget::Native(target)
                if target.window == wid
                    && target.instance == instance
                    && target.view == view
                    && target.generation == generation
                    && target.action == crate::native_settings::route_action(
                        crate::native_settings::SettingsRoute::Themes
                    )
        ));

        app.palette_activate();
        assert!(app.windows.get(&wid).unwrap().palette().is_none());
        assert!(matches!(
            app.native_runtime.view_state(view),
            Some(crate::native_app::AppViewState::Settings(state))
                if state.route == crate::native_settings::SettingsRoute::Themes
        ));
    }

    #[test]
    fn stale_native_palette_target_cannot_redirect_after_tab_switch() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (_, settings_view) = app.active_native_view(wid).expect("Settings active");
        let before = match app.native_runtime.view_state(settings_view).unwrap() {
            crate::native_app::AppViewState::Settings(state) => state.route,
            _ => panic!("Settings view"),
        };
        app.palette_enter();
        filter(&mut app, "Settings: Go to Themes");
        let target = match app
            .windows
            .get(&wid)
            .and_then(|window| window.palette())
            .and_then(crate::palette::PaletteState::selected_target)
        {
            Some(crate::palette::PaletteTarget::Native(target)) => target,
            other => panic!("expected native target, got {other:?}"),
        };

        app.switch_tab_in(wid, 0);
        assert!(app.active_native_view(wid).is_none(), "terminal is active");
        assert!(
            app.windows[&wid]
                .palette()
                .unwrap()
                .controls_lines()
                .iter()
                .all(|line| !line.contains("target=native")),
            "terminal switch removes app-scoped rows immediately"
        );
        assert!(
            app.activate_native_palette_target(&target).is_err(),
            "the captured Settings target is stale"
        );
        assert!(
            matches!(
                app.native_runtime.view_state(settings_view),
                Some(crate::native_app::AppViewState::Settings(state)) if state.route == before
            ),
            "no action reaches the parked native view"
        );
    }

    #[test]
    fn terminal_palette_has_no_native_rows_and_closed_controls_match_native_snapshot() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.palette_enter();
        assert!(
            app.windows[&wid]
                .palette()
                .unwrap()
                .controls_lines()
                .iter()
                .all(|line| !line.contains("target=native")),
            "terminal tabs do not inherit parked native commands"
        );
        app.palette_exit();

        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let closed = app.read_aux_controls(crate::app_introspect::AuxTarget::Menu);
        assert!(closed.iter().any(|line| {
            line.contains("section=\"Settings App\"")
                && line.contains("action=settings/route/themes")
        }));
        app.palette_enter();
        assert_eq!(
            closed,
            app.windows[&wid].palette().unwrap().controls_lines(),
            "closed controls and the painted open surface share one row constructor"
        );
    }

    #[test]
    fn terminal_only_split_and_session_window_commands_are_disabled_on_native_tabs() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let actions = ["SplitVertical", "SplitHorizontal", "ViewSessionInNewWindow"];
        let enabled = |palette: &crate::palette::PaletteState, action: &str| {
            palette.controls_lines().iter().any(|line| {
                line.contains(&format!("target=menu action={action} "))
                    && line.contains("enabled=true")
            })
        };

        let terminal = app.palette_snapshot(wid);
        for action in actions {
            assert!(
                enabled(&terminal, action),
                "{action} enabled on terminal tab"
            );
        }

        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let native = app.palette_snapshot(wid);
        for action in actions {
            assert!(
                !enabled(&native, action),
                "{action} disabled on native whole tab"
            );
            assert!(
                native.controls_lines().iter().any(|line| line
                    .contains(&format!("target=menu action={action} "))
                    && line.contains("enabled=false")),
                "{action} remains discoverable"
            );
        }
    }

    /// Headless, the local-file rows read disabled; windowed, the platform predicate decides.
    #[test]
    fn local_file_open_rows_are_disabled_headless() {
        let mut app = App::headless_for_test();
        assert!(app.headless, "the fixture is headless");
        assert!(
            !app.palette_live().local_file_picker_available,
            "no picker on a headless instance"
        );
        app.headless = false;
        assert_eq!(
            app.palette_live().local_file_picker_available,
            crate::menu::local_file_picker_available(),
            "windowed: the platform predicate decides"
        );
    }

    #[test]
    fn disabled_native_command_is_visible_but_inert() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (_, view) = app.active_native_view(wid).unwrap();
        let before_focus = app
            .native_runtime
            .view_state(view)
            .and_then(|state| state.common().last_focus.clone());
        app.palette_enter();
        filter(&mut app, "Undo Last Change");
        let palette = app.windows[&wid].palette().unwrap();
        assert!(
            palette
                .controls_lines()
                .iter()
                .any(|line| line.contains("settings/undo") && line.contains("enabled=false"))
        );
        assert!(palette.selected_target().is_none());
        app.palette_activate();
        let focus = app
            .native_runtime
            .view_state(view)
            .and_then(|state| state.common().last_focus.clone());
        assert_eq!(focus, before_focus, "disabled command dispatches nothing");
        assert!(
            app.windows[&wid].palette().is_some(),
            "disabled activation leaves the command surface open"
        );
    }

    #[test]
    fn copy_enablement_uses_native_selection_and_never_the_hidden_terminal() {
        use aterm_core::selection::{SelectionSide, SelectionType};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let hidden_terminal = app
            .front_terminal(wid)
            .expect("front terminal before opening Settings")
            .term
            .clone();
        {
            let mut term = crate::term_lock(&hidden_terminal);
            term.process(b"hidden terminal text");
            let selection = term.text_selection_mut();
            selection.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
            selection.update_selection(0, 6, SelectionSide::Right);
            selection.complete_selection();
            assert!(selection.has_selection());
        }

        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        assert!(
            !app.palette_live().has_selection,
            "a hidden terminal selection cannot enable native Copy"
        );
        let (_, view) = app.active_native_view(wid).unwrap();
        let Some(crate::native_app::AppViewState::Settings(settings)) =
            app.native_runtime.view_state_mut(view)
        else {
            panic!("Settings view state");
        };
        settings.search_input.insert("native selection");
        settings.search_input.select_all();
        assert!(
            app.palette_live().has_selection,
            "the active Settings selection enables Copy"
        );

        let palette = app.palette_snapshot(wid);
        assert!(palette.controls_lines().iter().any(|line| {
            line.contains("target=menu action=Copy") && line.contains("enabled=true")
        }));
    }

    #[test]
    fn native_palette_is_visible_in_the_same_raster_that_swallows_input() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        assert!(app.prepare_native_input_scratch(wid));
        let (base_fp, base_rgba) = app.windows[&wid]
            .settings_card
            .as_ref()
            .map(|card| (card.fp, card.rgba.clone()))
            .expect("native surface raster");

        app.palette_enter();
        assert!(app.prepare_native_input_scratch(wid));
        let (open_fp, open_rgba) = app.windows[&wid]
            .settings_card
            .as_ref()
            .map(|card| (card.fp, card.rgba.clone()))
            .expect("native + palette raster");
        assert_ne!(
            open_fp, base_fp,
            "overlay state invalidates retained pixels"
        );
        assert_ne!(open_rgba, base_rgba, "the palette is actually composited");

        app.palette_input_event(wid, &crate::input::InputEvent::Text("search".to_string()));
        assert!(
            app.windows[&wid].palette().unwrap().controls_lines()[0].contains("query=\"search\"")
        );
        assert!(app.prepare_native_input_scratch(wid));
        let filtered_fp = app.windows[&wid].settings_card.as_ref().unwrap().fp;
        assert_ne!(
            filtered_fp, open_fp,
            "swallowed input changes visible pixels"
        );
    }

    /// The palette's scrim rides beside a card-sized raster as a colour, never
    /// baked into a window-sized one, so a keystroke rasterizes, compares and
    /// uploads the card alone; and the theme picker's card carries none, so its
    /// preview is seen undimmed (Windows; no scrim elsewhere).
    #[test]
    fn the_palette_scrim_is_a_layer_and_the_theme_picker_has_none() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        {
            let ws = app.windows.get_mut(&wid).unwrap();
            ws.cols = 200;
            ws.input_scratch.cells = vec![Vec::new(); 50];
        }
        app.palette_enter();
        app.splice_settings_panel(wid);
        let frame_w = 200 * app.win_cell_size(wid).0;
        let card = app.windows[&wid]
            .settings_card
            .as_ref()
            .expect("an open palette rasterizes a card");
        assert_eq!(card.scrim, crate::palette::palette_scrim());
        assert!(
            card.dx > 0 && (card.dx + card.pw) as usize <= frame_w,
            "the raster is the card's ({} px at {}), not the {frame_w} px window's",
            card.pw,
            card.dx
        );

        app.windows.get_mut(&wid).unwrap().overlay = None;
        app.open_theme_palette();
        assert!(
            app.windows[&wid]
                .palette()
                .is_some_and(|p| p.is_themes_only())
        );
        app.splice_settings_panel(wid);
        let card = app.windows[&wid]
            .settings_card
            .as_ref()
            .expect("the theme picker rasterizes a card");
        assert_eq!(card.scrim, None, "Choose Theme… previews undimmed");
    }

    /// A theme change that is not an OS appearance flip (a colour-only reload, a program's
    /// OSC 11 background) repaints an open card: its cache keys on the theme it paints with,
    /// since the palette model and geometry do not change.
    #[test]
    fn open_palette_card_re_rasterizes_on_a_live_theme_swap() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.windows.get_mut(&wid).unwrap().input_scratch.cells = vec![Vec::new(); 24];
        app.palette_enter();
        app.splice_settings_panel(wid);
        let card = |app: &App| {
            app.windows[&wid]
                .settings_card
                .as_ref()
                .map(|card| (card.fp, card.geom, card.rgba.clone()))
                .expect("an open palette rasterizes a card")
        };
        let (fp, geom, rgba) = card(&app);

        let old = app.theme;
        let swapped = aterm_render::Theme {
            fg: old.fg ^ 0x00ff_ffff,
            bg: old.bg ^ 0x00ff_ffff,
            cursor: old.cursor ^ 0x00ff_ffff,
            selection: old.selection ^ 0x00ff_ffff,
        };
        app.apply_theme_live(swapped);
        app.splice_settings_panel(wid);
        let (fp2, geom2, rgba2) = card(&app);
        assert_eq!(fp2, fp, "a theme swap does not touch the palette model");
        assert_ne!(geom2, geom, "the painted theme is part of the cache key");
        assert_ne!(rgba2, rgba, "the open card repaints in the new colours");

        app.windows.get_mut(&wid).unwrap().input_scratch.default_bg = 0x0012_3456;
        app.splice_settings_panel(wid);
        let (fp3, geom3, rgba3) = card(&app);
        assert_eq!(fp3, fp, "a program's background does not touch the model");
        assert_ne!(geom3, geom2, "the live background is part of the cache key");
        assert_ne!(
            rgba3, rgba2,
            "the open card re-tints off the new background"
        );
    }

    use crate::control_theme::PreviewHold;
    use crate::palette::PaletteTarget;

    /// A headless App drawing `toml`, installed as a config reload would.
    fn themed_app(toml: &str) -> App {
        let mut app = App::headless_for_test();
        app.config = aterm_toml::from_str(toml).expect("valid toml");
        app.install_resolved_colors(crate::app_colorscheme::ThemeChange::ConfigReload);
        app
    }

    /// The durable side of a theme: the config lane's text and revision, the
    /// requests queued for it, and the in-memory `theme`.
    fn durable(app: &App) -> (String, u64, usize, Option<String>) {
        let snapshot = app.native_config_service.snapshot();
        (
            snapshot.text.to_string(),
            snapshot.revision,
            app.native_config_pending.len(),
            app.config.theme.clone(),
        )
    }

    fn front_palette(app: &App) -> &crate::palette::PaletteState {
        app.windows[&WindowId(0)]
            .palette()
            .expect("the palette is open")
    }

    /// Move the cursor onto the first filtered row whose target is `id`.
    fn highlight(app: &mut App, id: &str) {
        highlight_in(app, WindowId(0), id);
    }

    /// [`highlight`] in window `wid`'s palette, with `wid` brought to the
    /// front (the palette keys act on the front window).
    fn highlight_in(app: &mut App, wid: WindowId, id: &str) {
        app.frontmost_window = Some(wid);
        fn front_palette(app: &App, wid: WindowId) -> &crate::palette::PaletteState {
            app.windows[&wid].palette().expect("the palette is open")
        }
        let at = {
            let palette = front_palette(app, wid);
            palette
                .filtered()
                .iter()
                .position(|&i| palette.rows()[i].action == PaletteTarget::Theme(id.to_string()))
                .unwrap_or_else(|| panic!("{id} is a row"))
        };
        let selected = {
            let palette = front_palette(app, wid);
            palette
                .filtered()
                .iter()
                .position(|&i| {
                    Some(&palette.rows()[i].action) == palette.selected_target().as_ref()
                })
                .unwrap_or(0)
        };
        app.palette_move(at as isize - selected as isize);
    }

    /// `gruv` in the palette lists exactly the two Gruvbox themes among its
    /// Theme rows, labelled by their names, their appearance the hint.
    #[test]
    fn palette_theme_filter_lists_both_gruvbox_rows() {
        let mut app = themed_app("theme = \"Dracula\"\n");
        app.palette_enter();
        filter(&mut app, "gruv");
        let palette = front_palette(&app);
        let themes: Vec<(String, String)> = palette
            .filtered()
            .iter()
            .map(|&i| &palette.rows()[i])
            .filter(|row| matches!(row.action, PaletteTarget::Theme(_)))
            .map(|row| (row.label.to_string(), row.shortcut.to_string()))
            .collect();
        assert_eq!(
            themes,
            [
                ("Gruvbox Dark".to_string(), "dark".to_string()),
                ("Gruvbox Light".to_string(), "light".to_string()),
            ]
        );
        app.palette_exit();
        app.palette_enter();
        filter(&mut app, "light");
        let palette = front_palette(&app);
        let light: Vec<&str> = palette
            .filtered()
            .iter()
            .map(|&i| &palette.rows()[i])
            .filter(|row| matches!(row.action, PaletteTarget::Theme(_)))
            .map(|row| &*row.shortcut)
            .collect();
        assert!(!light.is_empty());
        assert!(light.iter().all(|hint| *hint == "light"), "{light:?}");
    }

    /// Highlighting a theme draws it everywhere once the turn's sync runs, and
    /// writes nothing; Esc puts back exactly what was drawn, config untouched.
    #[test]
    fn palette_theme_highlight_previews_and_esc_reverts() {
        let mut app = themed_app("theme = \"Dracula\"\n");
        let before = app.theme;
        let durable_before = durable(&app);
        app.palette_enter();
        filter(&mut app, "gruv");
        highlight(&mut app, "Gruvbox Light");
        assert_eq!(app.theme, before, "the highlight is read once per turn");
        let fanouts = app.engine_color_fanouts;
        app.sync_palette_theme_preview();
        let light = aterm_types::scheme::builtin("Gruvbox Light").unwrap();
        assert_eq!(app.theme, aterm_render::Theme::from(&light));
        assert_eq!(app.engine_color_fanouts, fanouts + 1, "one install");
        assert_eq!(
            app.theme_preview.as_ref().map(|p| (p.id.as_str(), p.hold)),
            Some(("Gruvbox Light", PreviewHold::Palette))
        );
        assert_eq!(
            app.next_theme_preview_wake(),
            None,
            "a palette preview has no TTL"
        );
        app.sync_palette_theme_preview();
        assert_eq!(app.engine_color_fanouts, fanouts + 1);
        assert_eq!(durable(&app), durable_before, "a preview writes nothing");

        highlight(&mut app, "Gruvbox Dark");
        app.sync_palette_theme_preview();
        let dark = aterm_types::scheme::builtin("Gruvbox Dark").unwrap();
        assert_eq!(app.theme, aterm_render::Theme::from(&dark));

        app.palette_input_event(
            WindowId(0),
            &crate::input::InputEvent::Key {
                key: aterm_types::keyboard::Key::Named(aterm_types::keyboard::NamedKey::Escape),
                mods: aterm_types::keyboard::Modifiers::empty(),
                base_layout: None,
                layout_text: None,
                event_type: aterm_types::keyboard::KeyEventType::Press,
            },
        );
        assert!(app.windows[&WindowId(0)].palette().is_none(), "Esc closes");
        assert!(app.theme_preview.is_none(), "Esc ends the preview");
        assert_eq!(app.theme, before, "Esc restores the configured theme");
        assert_eq!(durable(&app), durable_before, "and writes nothing");
    }

    /// Spec `session-theme-install`: Choose Tab Theme… draws the highlighted
    /// theme on the front session only — no instance preview, nothing written
    /// — Esc puts the tab's previous theme back, and Enter keeps the row as
    /// the tab's own theme.
    #[test]
    fn choose_tab_theme_draws_on_the_tab_and_esc_or_enter_settle_it() {
        let mut app = themed_app("theme = \"Dracula\"\n");
        let instance = app.theme;
        let durable_before = durable(&app);
        let bg = |app: &App| {
            crate::term_lock(&app.pool.get(0).expect("session 0").term).default_background()
        };
        let dracula = aterm_types::scheme::builtin("Dracula").unwrap().background;
        let light = aterm_types::scheme::builtin("Gruvbox Light")
            .unwrap()
            .background;
        assert_eq!(bg(&app), dracula);
        assert!(!app.palette_live().tab_theme, "nothing to clear yet");

        app.open_tab_theme_palette();
        highlight(&mut app, "Gruvbox Light");
        app.sync_palette_theme_preview();
        assert_eq!(bg(&app), light, "drawn on the tab");
        assert_eq!(app.theme, instance, "the window's theme is untouched");
        assert!(app.theme_preview.is_none(), "no instance preview");
        let esc = crate::input::InputEvent::Key {
            key: aterm_types::keyboard::Key::Named(aterm_types::keyboard::NamedKey::Escape),
            mods: aterm_types::keyboard::Modifiers::empty(),
            base_layout: None,
            layout_text: None,
            event_type: aterm_types::keyboard::KeyEventType::Press,
        };
        app.palette_input_event(WindowId(0), &esc);
        app.sync_palette_theme_preview();
        assert_eq!(bg(&app), dracula, "Esc puts the tab's theme back");
        assert!(app.pool.theme(0).is_none());

        app.open_tab_theme_palette();
        highlight(&mut app, "Gruvbox Light");
        app.sync_palette_theme_preview();
        app.palette_activate();
        app.sync_palette_theme_preview();
        assert_eq!(bg(&app), light, "Enter keeps it");
        assert_eq!(
            app.pool.theme(0).map(|t| t.selector.as_str()),
            Some("Gruvbox Light")
        );
        assert!(app.palette_live().tab_theme);
        assert_eq!(durable(&app), durable_before, "nothing is written");

        app.clear_front_tab_theme();
        assert_eq!(bg(&app), dracula, "cleared, the window's theme");
        assert!(app.pool.theme(0).is_none());
    }

    /// Spec `session-theme-carry`: a layout captured while Choose Tab Theme…
    /// is open writes the tab's theme from before the trial, not the
    /// tentative highlight; once Enter keeps the row, the row is written.
    #[test]
    fn a_layout_captured_mid_trial_keeps_the_prior_tab_theme() {
        let mut app = themed_app("theme = \"Dracula\"\n");
        app.set_session_theme(
            0,
            Some(crate::session_theme::SessionTheme {
                selector: "Hearth".to_string(),
                origin: crate::session_theme::SessionThemeOrigin::Ctl,
            }),
        );
        let written = |app: &App| {
            app.capture_restore_manifest()
                .to_toml()
                .expect("serializes")
        };
        app.open_tab_theme_palette();
        highlight(&mut app, "Gruvbox Light");
        app.sync_palette_theme_preview();
        assert_eq!(
            app.pool.theme(0).map(|t| t.selector.as_str()),
            Some("Gruvbox Light"),
            "PRECONDITION: the highlight draws on the tab"
        );
        let mid = written(&app);
        assert!(mid.contains("theme = \"Hearth\""), "{mid}");
        assert!(!mid.contains("Gruvbox Light"), "{mid}");
        app.palette_activate();
        app.sync_palette_theme_preview();
        let kept = written(&app);
        assert!(kept.contains("theme = \"Gruvbox Light\""), "{kept}");
    }

    /// Spec `session-theme-install`: a Choose Tab Theme… trial belongs to the
    /// window whose palette opened it.
    #[test]
    fn a_tab_theme_trial_answers_only_to_its_own_window() {
        let mut app = themed_app("theme = \"Dracula\"\n");
        let (a, b) = (WindowId(0), WindowId(1));
        let bg = |app: &App| {
            crate::term_lock(&app.pool.get(0).expect("session 0").term).default_background()
        };
        let light = aterm_types::scheme::builtin("Gruvbox Light")
            .unwrap()
            .background;
        app.install_window_state(b, crate::stub_session(99), 38, 132);
        app.palette_enter();
        assert!(
            app.windows[&b].palette().is_some(),
            "an ordinary palette in B"
        );

        app.frontmost_window = Some(a);
        app.open_tab_theme_palette();
        highlight_in(&mut app, a, "Gruvbox Light");
        app.sync_palette_theme_preview();
        assert_eq!(bg(&app), light, "A's highlight draws on its tab");

        app.frontmost_window = Some(b);
        filter(&mut app, "nord");
        highlight_in(&mut app, b, "Nord");
        app.sync_palette_theme_preview();
        assert_eq!(bg(&app), light, "B's highlight leaves A's tab alone");
        assert_eq!(
            app.theme_preview
                .as_ref()
                .map(|preview| preview.id.as_str()),
            Some("Nord"),
            "B previews on the instance"
        );
        app.palette_activate();
        app.sync_palette_theme_preview();
        assert_eq!(
            app.pool.theme(0).map(|t| t.selector.as_str()),
            Some("Gruvbox Light"),
            "Enter in B does not settle A's trial"
        );
        assert!(app.palette_tab_theme.is_some(), "A's trial stands");
        assert!(
            app.has_live_message("Couldn't save the theme"),
            "Enter in B tries to write the instance theme (refused headless)"
        );

        app.palette_enter();
        assert!(app.windows[&b].palette().is_some());
        app.frontmost_window = Some(a);
        let esc = crate::input::InputEvent::Key {
            key: aterm_types::keyboard::Key::Named(aterm_types::keyboard::NamedKey::Escape),
            mods: aterm_types::keyboard::Modifiers::empty(),
            base_layout: None,
            layout_text: None,
            event_type: aterm_types::keyboard::KeyEventType::Press,
        };
        app.palette_input_event(a, &esc);
        app.sync_palette_theme_preview();
        assert!(app.palette_tab_theme.is_none(), "Esc in A ends its trial");
        assert!(app.pool.theme(0).is_none(), "and puts the tab's theme back");
        assert_ne!(bg(&app), light);
    }

    /// Opening a second Tab-theme trial must retire the first trial rather
    /// than make its highlight permanent.
    #[test]
    fn a_second_tab_theme_trial_reverts_the_first_windows_unkept_highlight() {
        let setup = || {
            let mut app = themed_app("theme = \"Dracula\"\n");
            app.install_window_state(WindowId(1), crate::stub_session(99), 38, 132);
            app.frontmost_window = Some(WindowId(0));
            app.set_session_theme(
                0,
                Some(crate::session_theme::SessionTheme {
                    selector: "Hearth".into(),
                    origin: crate::session_theme::SessionThemeOrigin::Ctl,
                }),
            );
            app
        };
        let mut kept = setup();
        kept.open_tab_theme_palette();
        highlight_in(&mut kept, WindowId(0), "Gruvbox Light");
        kept.sync_palette_theme_preview();
        kept.palette_activate();
        kept.sync_palette_theme_preview();
        assert!(
            kept.palette_tab_theme.is_none(),
            "Enter settled the first trial"
        );
        kept.frontmost_window = Some(WindowId(1));
        kept.open_tab_theme_palette();
        assert_eq!(
            kept.pool.theme(0).map(|t| t.selector.as_str()),
            Some("Gruvbox Light")
        );

        let mut app = setup();
        let before = durable(&app);
        app.open_tab_theme_palette();
        highlight_in(&mut app, WindowId(0), "Gruvbox Light");
        app.sync_palette_theme_preview();
        assert_eq!(
            app.pool.theme(0).map(|t| t.selector.as_str()),
            Some("Gruvbox Light"),
            "the first highlight actually draws"
        );
        assert_eq!(
            app.kept_session_theme(0).map(|t| t.selector.as_str()),
            Some("Hearth"),
            "the first trial is still tentative"
        );
        app.frontmost_window = Some(WindowId(1));
        app.open_tab_theme_palette();
        assert_eq!(
            app.palette_tab_theme.as_ref().map(|t| t.window),
            Some(WindowId(1)),
            "the second trial actually opens"
        );
        assert!(app.windows[&WindowId(1)].palette().is_some());
        assert_eq!(durable(&app), before, "the trials write no config");
        assert_eq!(
            (
                app.pool.theme(0).map(|t| t.selector.as_str()),
                app.kept_session_theme(0).map(|t| t.selector.as_str()),
                app.windows[&WindowId(0)].palette().is_none()
            ),
            (Some("Hearth"), Some("Hearth"), true),
            "the first trial is reverted and closed, not silently kept",
        );
    }

    /// A highlight that leaves the Theme section, or a palette closed some other way (another
    /// overlay took the slot), reverts at the next turn's sync.
    #[test]
    fn palette_theme_preview_ends_when_the_highlight_or_the_palette_goes() {
        let mut app = themed_app("theme = \"Dracula\"\n");
        let before = app.theme;
        app.palette_enter();
        filter(&mut app, "gruv");
        highlight(&mut app, "Gruvbox Light");
        app.sync_palette_theme_preview();
        assert_ne!(app.theme, before);
        for _ in 0..4 {
            // The cursor returns to the first (menu) row.
            app.palette_edit(crate::app_search::SearchEdit::DeleteBack);
        }
        assert!(
            front_palette(&app)
                .selected_target()
                .is_some_and(|target| !matches!(target, PaletteTarget::Theme(_)))
        );
        app.sync_palette_theme_preview();
        assert!(app.theme_preview.is_none());
        assert_eq!(
            app.theme, before,
            "off the Theme section, nothing is previewed"
        );

        filter(&mut app, "gruv");
        highlight(&mut app, "Gruvbox Light");
        app.sync_palette_theme_preview();
        assert_ne!(app.theme, before);
        app.windows.get_mut(&WindowId(0)).unwrap().overlay = None;
        app.sync_palette_theme_preview();
        assert!(
            app.theme_preview.is_none(),
            "no palette, no palette preview"
        );
        assert_eq!(app.theme, before);
    }

    /// Negative control: moving over rows that are not themes never touches the
    /// preview lane, and a `theme preview` from the socket survives a palette
    /// whose highlight did not move.
    #[test]
    fn palette_non_theme_rows_leave_the_preview_lane_alone() {
        let mut app = themed_app("theme = \"Dracula\"\n");
        let before = app.theme;
        app.palette_enter();
        for _ in 0..5 {
            app.palette_move(1);
            app.sync_palette_theme_preview();
            assert!(
                !matches!(
                    front_palette(&app).selected_target(),
                    Some(PaletteTarget::Theme(_))
                ),
                "{:#?}",
                front_palette(&app).controls_lines()
            );
            assert!(app.theme_preview.is_none());
            assert_eq!(app.theme, before);
        }
        let now = std::time::Instant::now();
        app.preview_theme("Nord", std::time::Duration::from_secs(60), now)
            .expect("a built-in previews");
        app.palette_move(1);
        app.sync_palette_theme_preview();
        assert_eq!(
            app.theme_preview.as_ref().map(|p| p.id.as_str()),
            Some("Nord"),
            "the palette does not fight a control-socket preview"
        );
        app.palette_exit();
        assert_eq!(
            app.theme_preview.as_ref().map(|p| p.id.as_str()),
            Some("Nord"),
            "closing the palette ends only a palette-held preview"
        );
    }

    /// Enter on a Theme row attempts exactly one write and keeps nothing it
    /// could not save: headless has no event-loop proxy, so the one attempt is
    /// refused, said once, and the screen returns to the configured theme.
    #[test]
    fn palette_theme_enter_attempts_one_write_and_reverts_when_refused() {
        let mut app = themed_app("theme = \"Dracula\"\n");
        let before = app.theme;
        let durable_before = durable(&app);
        app.palette_enter();
        filter(&mut app, "gruv");
        highlight(&mut app, "Gruvbox Light");
        app.sync_palette_theme_preview();
        app.palette_activate();
        assert!(
            app.windows[&WindowId(0)].palette().is_none(),
            "Enter closes"
        );
        assert!(app.has_live_message("Couldn't save the theme"));
        assert!(app.theme_preview.is_none(), "an unsaved choice is not kept");
        assert_eq!(app.theme, before);
        assert_eq!(durable(&app), durable_before);
    }

    /// View ▸ Choose Theme… (and the `choose_theme` key) opens the palette scoped to its Theme
    /// section, the cursor on the configured theme, which is not re-installed.
    #[test]
    fn choose_theme_opens_the_scoped_palette_on_the_configured_theme() {
        let mut app = themed_app("theme = \"Nord\"\n");
        let fanouts = app.engine_color_fanouts;
        app.dispatch_action(WindowId(0), crate::keybinding::Action::ChooseTheme);
        let palette = front_palette(&app);
        assert!(palette.is_themes_only());
        assert_eq!(palette.highlighted_theme(), Some("Nord"));
        let scoped = palette.filtered();
        let (browse, themes) = scoped.split_last().expect("rows");
        assert!(
            themes
                .iter()
                .all(|&i| matches!(palette.rows()[i].action, PaletteTarget::Theme(_)))
        );
        assert!(
            matches!(
                palette.rows()[*browse].action,
                PaletteTarget::Menu(crate::menu::MenuAction::BrowseThemes)
            ),
            "the scoped palette ends on Browse all themes…"
        );
        assert!(palette.controls_lines()[0].ends_with("scope=themes"));
        app.sync_palette_theme_preview();
        assert!(app.theme_preview.is_none(), "opening previews nothing");
        assert_eq!(app.engine_color_fanouts, fanouts);
        app.open_theme_palette();
        assert!(front_palette(&app).is_themes_only());
        assert_eq!(
            crate::keybinding::Action::parse("choose_theme"),
            Some(crate::keybinding::Action::ChooseTheme)
        );
        assert!(crate::keybinding::ACTION_NAMES.contains(&"choose_theme"));
    }

    /// A press shaped like the platform's, through the shipping `on_key`.
    fn press_key(
        app: &mut App,
        mods: aterm_window::keyboard::ModifiersState,
        code: aterm_window::keyboard::KeyCode,
        logical: aterm_window::keyboard::Key,
        text: Option<&str>,
        repeat: bool,
    ) {
        use aterm_window::event::{ElementState, KeyEvent};
        use aterm_window::keyboard::{KeyLocation, PhysicalKey, SmolStr};
        app.windows.get_mut(&WindowId(0)).expect("window").mods = mods;
        app.on_key(
            WindowId(0),
            KeyEvent::synthetic_for_test(
                PhysicalKey::Code(code),
                logical,
                text.map(SmolStr::new),
                KeyLocation::Standard,
                ElementState::Pressed,
                repeat,
            ),
        );
    }

    fn release_key(app: &mut App, code: aterm_window::keyboard::KeyCode) {
        app.release_physical_press(WindowId(0), aterm_window::keyboard::PhysicalKey::Code(code));
    }

    /// A Ctrl+letter (`ctrl` = true) or a plain typed letter, pressed and released.
    fn letter(app: &mut App, ctrl: bool, letter: &str) {
        use aterm_window::keyboard::{Key, KeyCode, ModifiersState, SmolStr};
        let mods = if ctrl {
            ModifiersState::CONTROL
        } else {
            ModifiersState::empty()
        };
        press_key(
            app,
            mods,
            KeyCode::KeyA,
            Key::Character(SmolStr::new(letter)),
            Some(letter),
            false,
        );
        release_key(app, KeyCode::KeyA);
    }

    fn typed(app: &mut App, text: &str) {
        for c in text.chars() {
            letter(app, false, &c.to_string());
        }
    }

    fn query(app: &App) -> String {
        front_palette(app).query().to_string()
    }

    /// THE PALETTE FILTER HONOURS READLINE EDITING, like the find bar and the inline
    /// rename (one keymap, `app_input::field_edit_action`).
    #[test]
    fn the_filter_takes_the_readline_chords_instead_of_typing_their_letters() {
        let mut app = App::headless_for_test();
        app.palette_enter();
        typed(&mut app, "new tab");
        assert_eq!(query(&app), "new tab");

        for motion in ["a", "e", "b", "f", "k", "d"] {
            letter(&mut app, true, motion);
            assert_eq!(query(&app), "new tab", "^{motion} types nothing");
        }
        letter(&mut app, true, "w");
        assert_eq!(query(&app), "new ", "^W deletes the word before the caret");
        letter(&mut app, true, "h");
        assert_eq!(query(&app), "new", "^H deletes a character");
        typed(&mut app, " win");
        letter(&mut app, true, "u");
        assert_eq!(query(&app), "", "^U clears the filter");
        typed(&mut app, "ab");
        for unclaimed in ["n", "p", "g"] {
            letter(&mut app, true, unclaimed);
        }
        assert_eq!(query(&app), "ab", "^N/^P/^G type nothing");
        assert!(
            app.windows[&WindowId(0)].palette().is_some(),
            "and close nothing"
        );
    }

    /// Typing, Backspace, the word kills, Up/Down, Enter and Escape keep their
    /// meaning, and Ctrl WITH Alt still types what it composes (Windows'
    /// LeftCtrl+LeftAlt spelling of AltGr: de-DE `@`).
    #[test]
    fn typing_navigation_and_the_named_keys_still_drive_the_palette() {
        use aterm_window::keyboard::{Key, KeyCode, ModifiersState, NamedKey, SmolStr};
        let mut app = App::headless_for_test();
        app.palette_enter();
        typed(&mut app, "copy");
        press_key(
            &mut app,
            ModifiersState::CONTROL | ModifiersState::ALT,
            KeyCode::KeyQ,
            Key::Character(SmolStr::new("@")),
            Some("@"),
            false,
        );
        release_key(&mut app, KeyCode::KeyQ);
        assert_eq!(query(&app), "copy@", "Ctrl+Alt composes, as AltGr does");
        let backspace = |app: &mut App, mods| {
            press_key(
                app,
                mods,
                KeyCode::Backspace,
                Key::Named(NamedKey::Backspace),
                Some("\u{8}"),
                false,
            );
            release_key(app, KeyCode::Backspace);
        };
        backspace(&mut app, ModifiersState::empty());
        assert_eq!(query(&app), "copy");
        typed(&mut app, " sel");
        backspace(&mut app, ModifiersState::ALT);
        assert_eq!(query(&app), "copy ", "⌥⌫ deletes a word");
        backspace(&mut app, ModifiersState::empty());
        assert_eq!(query(&app), "copy");

        let arrow = |app: &mut App, code, key: NamedKey| {
            press_key(
                app,
                ModifiersState::empty(),
                code,
                Key::Named(key),
                None,
                false,
            );
            release_key(app, code);
        };
        arrow(&mut app, KeyCode::ArrowLeft, NamedKey::ArrowLeft);
        assert_eq!(query(&app), "copy");

        let named = |app: &mut App, code, key: NamedKey, text: &str| {
            press_key(
                app,
                ModifiersState::empty(),
                code,
                Key::Named(key),
                Some(text),
                false,
            );
            release_key(app, code);
        };
        named(&mut app, KeyCode::Escape, NamedKey::Escape, "\u{1b}");
        assert!(app.windows[&WindowId(0)].palette().is_none(), "Esc closes");

        app.palette_enter();
        let selected = |app: &App| front_palette(app).controls_lines()[0].clone();
        assert!(
            selected(&app).contains(" selected=0 "),
            "{}",
            selected(&app)
        );
        arrow(&mut app, KeyCode::ArrowDown, NamedKey::ArrowDown);
        assert!(
            selected(&app).contains(" selected=1 "),
            "↓ {}",
            selected(&app)
        );
        arrow(&mut app, KeyCode::ArrowUp, NamedKey::ArrowUp);
        assert!(
            selected(&app).contains(" selected=0 "),
            "↑ {}",
            selected(&app)
        );

        typed(&mut app, "new window");
        assert!(
            front_palette(&app).selected_target().is_some(),
            "an enabled row is selected: {:?}",
            front_palette(&app).controls_lines()
        );
        named(&mut app, KeyCode::Enter, NamedKey::Enter, "\r");
        assert!(
            app.windows[&WindowId(0)].palette().is_none(),
            "Enter runs the row and closes"
        );
    }

    /// A held chord repeats what its press did: a held `^W` keeps deleting words.
    #[test]
    fn a_held_readline_chord_repeats_its_edit_not_its_letter() {
        use aterm_window::keyboard::{Key, KeyCode, ModifiersState, SmolStr};
        let mut app = App::headless_for_test();
        app.palette_enter();
        typed(&mut app, "one two three");
        let ctrl_w = |app: &mut App, repeat| {
            press_key(
                app,
                ModifiersState::CONTROL,
                KeyCode::KeyW,
                Key::Character(SmolStr::new("w")),
                Some("w"),
                repeat,
            );
        };
        ctrl_w(&mut app, false);
        assert_eq!(query(&app), "one two ");
        ctrl_w(&mut app, true);
        assert_eq!(query(&app), "one ", "the repeat deleted another word");
        ctrl_w(&mut app, true);
        assert_eq!(query(&app), "");
        release_key(&mut app, KeyCode::KeyW);
        let ctrl_n = |app: &mut App, repeat| {
            press_key(
                app,
                ModifiersState::CONTROL,
                KeyCode::KeyN,
                Key::Character(SmolStr::new("n")),
                Some("n"),
                repeat,
            );
        };
        ctrl_n(&mut app, false);
        ctrl_n(&mut app, true);
        release_key(&mut app, KeyCode::KeyN);
        assert_eq!(query(&app), "");
    }

    /// A DRIVEN PALETTE EDITS ITS FILTER AS A TYPED ONE DOES.
    #[test]
    fn a_driven_palette_edits_its_filter_as_a_typed_one_does() {
        use aterm_window::keyboard::{Key, KeyCode, ModifiersState as M, NamedKey, SmolStr};
        let ch = |s: &str| Key::Character(SmolStr::new(s));
        let named = Key::Named;
        let start = "one two";
        let cases = [
            (
                "ctrl+w",
                M::CONTROL,
                KeyCode::KeyW,
                ch("w"),
                Some("w"),
                "one ",
            ),
            ("ctrl+u", M::CONTROL, KeyCode::KeyU, ch("u"), Some("u"), ""),
            (
                "ctrl+h",
                M::CONTROL,
                KeyCode::KeyH,
                ch("h"),
                Some("h"),
                "one tw",
            ),
            (
                "ctrl+e",
                M::CONTROL,
                KeyCode::KeyE,
                ch("e"),
                Some("e"),
                start,
            ),
            (
                "ctrl+n",
                M::CONTROL,
                KeyCode::KeyN,
                ch("n"),
                Some("n"),
                start,
            ),
            (
                "alt+backspace",
                M::ALT,
                KeyCode::Backspace,
                named(NamedKey::Backspace),
                Some("\u{8}"),
                "one ",
            ),
            (
                "backspace",
                M::empty(),
                KeyCode::Backspace,
                named(NamedKey::Backspace),
                Some("\u{8}"),
                "one tw",
            ),
            (
                "left",
                M::empty(),
                KeyCode::ArrowLeft,
                named(NamedKey::ArrowLeft),
                None,
                start,
            ),
            (
                "space",
                M::empty(),
                KeyCode::Space,
                named(NamedKey::Space),
                Some(" "),
                "one two ",
            ),
            (
                "x",
                M::empty(),
                KeyCode::KeyX,
                ch("x"),
                Some("x"),
                "one twox",
            ),
            (
                "shift+x",
                M::SHIFT,
                KeyCode::KeyX,
                ch("X"),
                Some("X"),
                "one twoX",
            ),
            (
                "super+x",
                M::SUPER,
                KeyCode::KeyX,
                ch("x"),
                Some("x"),
                start,
            ),
        ];
        for (chord, mods, code, logical, text, want) in cases {
            let mut typed_app = App::headless_for_test();
            typed_app.palette_enter();
            typed(&mut typed_app, start);
            press_key(&mut typed_app, mods, code, logical, text, false);
            release_key(&mut typed_app, code);
            assert_eq!(query(&typed_app), want, "typed {chord}");

            let mut driven = App::headless_for_test();
            driven.palette_enter();
            typed(&mut driven, start);
            let event = crate::control::parse_key(chord).expect("a ctl key chord");
            assert_eq!(
                driven.input(WindowId(0), event, crate::input::Source::Controller),
                crate::input::InputOutcome::Ok,
                "the overlay swallows {chord}"
            );
            assert_eq!(query(&driven), want, "driven {chord}");
        }
    }

    /// The engine projection of the field keymap reads the arms a caret-less filter
    /// cannot show (the motions, ⌘⌫) as the winit projection does: a sample of each
    /// arm, and Ctrl+Alt (Windows' AltGr) composing its layout glyph in the palette
    /// rather than reading as a readline chord.
    #[test]
    fn the_engine_key_projection_reads_the_field_keymap() {
        use crate::app_input::field_edit_action_engine;
        use crate::app_search::SearchEdit;
        use aterm_types::keyboard::{Key as TKey, Modifiers, NamedKey as TNamed};
        let ctrl = Modifiers::CTRL;
        assert_eq!(
            field_edit_action_engine(&TKey::Character('a'), ctrl, None),
            Some(SearchEdit::MoveStart)
        );
        assert_eq!(
            field_edit_action_engine(&TKey::Character('w'), ctrl, None),
            Some(SearchEdit::DeleteWordBack)
        );
        assert_eq!(
            field_edit_action_engine(&TKey::Named(TNamed::ArrowLeft), Modifiers::ALT, None),
            Some(SearchEdit::MoveWordLeft)
        );
        assert_eq!(
            field_edit_action_engine(&TKey::Named(TNamed::Backspace), Modifiers::SUPER, None),
            Some(SearchEdit::KillToStart)
        );
        assert_eq!(
            field_edit_action_engine(&TKey::Named(TNamed::Home), Modifiers::empty(), None),
            Some(SearchEdit::MoveStart)
        );
        assert_eq!(
            field_edit_action_engine(&TKey::Character('q'), ctrl | Modifiers::ALT, Some('@')),
            None,
            "Ctrl+Alt is no readline chord and types nothing through the field arm"
        );
        assert_eq!(
            super::palette_input_key(&crate::input::InputEvent::Key {
                key: TKey::Character('q'),
                mods: ctrl | Modifiers::ALT,
                base_layout: None,
                layout_text: Some('@'),
                event_type: aterm_types::keyboard::KeyEventType::Press,
            }),
            Some(super::PaletteKey::Edit(SearchEdit::Insert("@".to_string()))),
            "the palette appends what Ctrl+Alt composes, as a typed AltGr does"
        );
        assert_eq!(
            field_edit_action_engine(&TKey::Character('é'), Modifiers::empty(), None),
            Some(SearchEdit::Insert("é".to_string()))
        );
    }
}
