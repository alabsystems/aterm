// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `App` glue for the in-window command palette ([`crate::palette`]).

use crate::App;
use crate::control_theme::PreviewHold;
use crate::palette::{
    NativeCommandScope, NativeCommandTarget, PaletteLive, PaletteState, PaletteTarget, PaletteTheme,
};

/// The `[keybindings]` action behind a menu action.
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

    /// Build a resolved [`PaletteState`] for window `wid`.
    pub(crate) fn palette_snapshot(&self, wid: crate::WindowId) -> PaletteState {
        let mut state = PaletteState::new();
        if let Some(scope) = self.native_palette_scope(wid) {
            state = state.with_native_commands(scope);
        }
        state = state.with_themes(self.palette_themes());
        state.resolve(&self.palette_live_for_window(Some(wid)));
        state
    }

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
        let Some(wid) = self.frontmost_window else {
            return;
        };
        self.open_theme_palette_in(wid);
    }

    /// Open the named window's command palette scoped to its Theme section.
    pub(crate) fn open_theme_palette_in(&mut self, wid: crate::WindowId) {
        let Some(window) = self.windows.get(&wid) else {
            return;
        };
        if window.palette().is_some_and(PaletteState::is_themes_only) {
            return;
        }
        if window.palette().is_some() {
            self.palette_exit_in(wid);
        }
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
            Some(Some(id)) if self.session_theme_unavailable(&id).is_none() => {
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
        let closed_owner = self.theme_preview.as_ref().and_then(|preview| {
            let PreviewHold::Palette { window } = preview.hold else {
                return None;
            };
            self.windows
                .get(&window)
                .is_none_or(|ws| ws.palette().is_none())
                .then_some(window)
        });
        if let Some(window) = closed_owner {
            self.end_theme_preview_if(|preview| preview.hold == PreviewHold::Palette { window });
        }
        let front = self.frontmost_window;
        let trial_window = self.palette_tab_theme.as_ref().map(|trial| trial.window);
        let mut trial_open = false;
        let mut trial_ask: Option<Option<String>> = None;
        let mut any_open = false;
        let mut ask: Option<(crate::WindowId, Option<String>)> = None;
        let mut front_ask: Option<(crate::WindowId, Option<String>)> = None;
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
                    front_ask = Some((wid, change));
                } else {
                    ask = Some((wid, change));
                }
            }
        }
        self.sync_tab_theme_trial(trial_open, trial_ask);
        let palette_held = |preview: &crate::control_theme::ThemePreview| {
            matches!(preview.hold, PreviewHold::Palette { .. })
        };
        if !any_open {
            self.end_theme_preview_if(palette_held);
            return;
        }
        match front_ask.or(ask) {
            None => {}
            Some((window, Some(id))) if !id.eq_ignore_ascii_case(self.configured_theme_id()) => {
                if let Err(why) = self.begin_theme_preview(&id, PreviewHold::Palette { window }) {
                    aterm_log::debug!("palette theme preview {id:?}: {why}");
                    self.end_theme_preview_if(palette_held);
                }
            }
            Some((window, _)) => {
                self.end_theme_preview_if(|preview| {
                    preview.hold == PreviewHold::Palette { window }
                });
            }
        }
    }

    fn configured_side_name(&self) -> String {
        crate::app_config::resolve_theme_name_value(
            self.config.theme.as_deref(),
            self.os_appearance,
        )
        .unwrap_or_else(|| "Default".to_string())
    }

    /// Commit theme `id` from the palette while holding [`PreviewHold::Committing`].
    #[cfg(test)]
    pub(crate) fn palette_choose_theme(&mut self, id: &str) {
        if let Some(wid) = self.frontmost_window {
            self.palette_choose_theme_in(wid, id);
        }
    }

    fn palette_choose_theme_in(&mut self, wid: crate::WindowId, id: &str) {
        if let Some(trial) = self.palette_tab_theme.take_if(|trial| trial.window == wid) {
            if self.pool.get(trial.session).is_none() {
                return;
            }
            let theme = match self.session_theme_unavailable(id) {
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
            self.end_theme_preview_if(|preview| {
                preview.hold == PreviewHold::Palette { window: wid }
            });
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

    /// Selection geometry on the screens the focused terminal and its visible siblings show.
    /// An unread lock stays unknown unless another pane proves a selection exists.
    fn terminal_selection_in(
        &self,
        wid: crate::WindowId,
        mut read: impl FnMut(&std::sync::Mutex<aterm_core::terminal::Terminal>) -> Option<bool>,
    ) -> Option<bool> {
        let Some(focused) = self.shown_front(wid) else {
            return Some(false);
        };
        let mut selected = read(&focused.term);
        if selected == Some(true)
            || self
                .windows
                .get(&wid)
                .and_then(|window| window.tab_set.active())
                .is_none_or(|tab| tab.root.len() <= 1)
        {
            return selected;
        }
        let Some(window) = self.windows.get(&wid) else {
            return selected;
        };
        if crate::active_tab_shows(window, |view| {
            let Some(crate::tab_model::View::Terminal(view)) = self.view_store.get(view) else {
                return false;
            };
            if view.session == focused.session {
                return false;
            }
            let Some(term) = self.pool.shown_term(view.session) else {
                return false;
            };
            match read(&term) {
                Some(true) => true,
                Some(false) => false,
                None => {
                    selected = None;
                    false
                }
            }
        }) {
            return Some(true);
        }
        selected
    }

    fn palette_live_for_window(&self, wid: Option<crate::WindowId>) -> PaletteLive {
        let window = wid.and_then(|wid| self.windows.get(&wid));
        let has_selection = wid.is_some_and(|wid| {
            if let Some(ready) = self.stats_lens_copy_ready(wid) {
                ready
            } else if self.active_native_view(wid).is_some() {
                // A private document's Copy is the person's keystroke ([`App::menu_copies`]).
                self.menu_copies(wid)
            } else {
                self.terminal_selection_in(wid, |term| {
                    Some(crate::term_lock(term).text_selection().has_selection())
                })
                .unwrap_or(false)
            }
        });
        let fullscreen = window
            .and_then(|ws| ws.os_window.as_ref())
            .and_then(|w| w.fullscreen())
            .is_some();
        let maximized = window
            .and_then(|ws| ws.os_window.as_ref())
            .is_some_and(|w| w.is_maximized());
        let multi_tab = window.is_some_and(|ws| ws.tab_set.shown_len() >= 2);
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
            can_rewind: wid.is_some_and(|wid| self.front_can_rewind(wid)),
            aos_attached: window.is_some_and(|ws| ws.fx.cursor().momentum.aos_attached()),
            aos_unseen: wid.is_some_and(|wid| self.aos_unseen_on(wid)),
            multi_tab,
            native_tab_active,
            terminal_front,
            can_reopen_closed_tab: self.can_reopen_closed_tab(),
            can_reopen_closed_view: self.can_reopen_closed_view(),
            local_file_picker_available: !self.headless
                && crate::menu::local_file_picker_available(),
            front_person_hold,
            front_remote_hold,
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

    /// Whether the front window's active tab is split.
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
        let content_window = front.filter(|wid| {
            self.windows
                .get(wid)
                .is_some_and(|window| !window.overlay_open())
        });
        let terminal = front.and_then(|wid| self.front_terminal(wid));
        let has_selection = if self.front().is_some_and(|window| window.overlay_open()) {
            false
        } else if self.front_rename_edit().is_some() {
            true
        } else if let Some(ready) = front.and_then(|wid| self.stats_lens_copy_ready(wid)) {
            ready
        } else if let Some(wid) = front.filter(|wid| self.active_native_view(*wid).is_some()) {
            self.menu_copies(wid)
        } else if let Some(wid) = front {
            self.terminal_selection_in(wid, |term| {
                crate::term_try_lock(term).map(|term| term.text_selection().has_selection())
            })
            .unwrap_or(last.has_selection)
        } else {
            false
        };
        let promotable = front.map_or(self.launch_kitty, |wid| self.promotable_kitty(wid));
        crate::menu::set_menu_live(crate::menu::MenuLive {
            has_selection,
            can_save: content_window.is_some_and(|wid| self.can_save_document(wid)),
            can_find: content_window.is_some_and(|wid| self.can_find(wid)),
            can_find_again: content_window.is_some_and(|wid| self.can_find_again(wid)),
            multi_tab: self.front().is_some_and(|ws| ws.tab_set.shown_len() >= 2),
            can_reopen_closed_tab: self.can_reopen_closed_tab(),
            can_reopen_closed_view: self.can_reopen_closed_view(),
            serious_mode: self.serious_mode_enabled(),
            rain_on: terminal.is_some_and(|t| self.session_rain_enabled(t.session)),
            kitty_favourited: self.kitty_log.is_favourite(promotable),
            front_tab_split: self.front_tab_split(),
            can_rewind: front.is_some_and(|wid| self.front_can_rewind(wid)),
            aos_attached: self
                .front()
                .is_some_and(|ws| ws.fx.cursor().momentum.aos_attached()),
            aos_unseen: front.is_some_and(|wid| self.aos_unseen_on(wid)),
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
            self.retire_native_input_in(wid);
            self.settle_pointer_drags(wid);
            let _ = self.palette_claims_pointer(wid);
            if let Some((x, y)) = self.windows.get(&wid).map(|window| window.last_cursor_px) {
                self.palette_pointer_motion(wid, x, y);
            }
        }
        #[cfg(a11y_tree)]
        self.push_a11y_tree(wid);
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
        self.retire_native_input_in(wid);
        self.settle_pointer_drags(wid);
        let _ = self.palette_claims_pointer(wid);
        self.invalidate_native_ui_cache(wid);
        #[cfg(a11y_tree)]
        self.push_a11y_tree(wid);
        Ok(())
    }

    /// Close the command palette on the front window.
    pub(crate) fn palette_exit(&mut self) {
        if let Some(wid) = self.frontmost_window {
            self.palette_exit_in(wid);
        }
    }

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
            self.end_theme_preview_if(|preview| {
                preview.hold == PreviewHold::Palette { window: wid }
            });
        }
        if closed {
            self.replay_held_pointer(wid);
        }
    }

    /// Toggle the command palette on the front window.
    pub(crate) fn toggle_palette(&mut self) {
        if let Some(wid) = self.frontmost_window {
            self.toggle_palette_in(wid);
        }
    }

    /// Toggle the command palette on the named window.
    pub(crate) fn toggle_palette_in(&mut self, wid: crate::WindowId) {
        let Some(window) = self.windows.get(&wid) else {
            return;
        };
        if window.palette().is_some() {
            self.palette_exit_in(wid);
        } else {
            let state = self.palette_snapshot(wid);
            self.palette_open_state(wid, state);
        }
    }

    /// Move the palette selection cursor by `delta` over the filtered rows.
    #[cfg(test)]
    pub(crate) fn palette_move(&mut self, delta: isize) {
        if let Some(wid) = self.frontmost_window {
            self.palette_move_in(wid, delta);
        }
    }

    fn palette_move_in(&mut self, wid: crate::WindowId, delta: isize) {
        if let Some(p) = self.windows.get_mut(&wid).and_then(|ws| ws.palette_mut()) {
            p.move_selection(delta);
        }
        self.palette_repaint_in(wid);
    }

    /// Append filter character `c` and re-filter the open palette.
    #[cfg(test)]
    pub(crate) fn palette_filter_push(&mut self, c: char) {
        if let Some(p) = self.front_mut().and_then(|ws| ws.palette_mut()) {
            p.push_char(c);
        }
        if let Some(wid) = self.frontmost_window {
            self.palette_repaint_in(wid);
        }
    }

    /// Activate the currently selected palette target.
    #[cfg(test)]
    pub(crate) fn palette_activate(&mut self) {
        if let Some(wid) = self.frontmost_window {
            self.palette_activate_in(wid);
        }
    }

    pub(crate) fn palette_activate_in(&mut self, wid: crate::WindowId) {
        let Some(target) = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.palette())
            .and_then(PaletteState::selected_target)
        else {
            return;
        };
        let content = self
            .windows
            .get(&wid)
            .and_then(|window| window.front_content);
        if let PaletteTarget::Theme(id) = target {
            self.palette_choose_theme_in(wid, &id);
            self.palette_exit_in(wid);
            return;
        }
        self.palette_exit_in(wid);
        if self
            .windows
            .get(&wid)
            .is_none_or(|window| window.front_content != content)
        {
            self.pending_action_refusal = Some("The palette's active content changed".to_string());
            return;
        }
        match target {
            PaletteTarget::Menu(crate::menu::MenuAction::TakeOver) => {
                self.take_over(wid);
            }
            PaletteTarget::Menu(crate::menu::MenuAction::HandBack) => {
                self.hand_back(wid);
            }
            PaletteTarget::Menu(crate::menu::MenuAction::Find) => {
                self.clear_move_license_from(wid, crate::cursor_glow::BoundaryOrigin::Palette);
                self.find_requested_in(wid);
            }
            PaletteTarget::Menu(
                action @ (crate::menu::MenuAction::FindNext | crate::menu::MenuAction::FindPrev),
            ) => {
                self.clear_move_license_from(wid, crate::cursor_glow::BoundaryOrigin::Palette);
                if let Err(why) =
                    self.search_find_again_in(wid, action == crate::menu::MenuAction::FindNext)
                {
                    self.pending_action_refusal = Some(why.to_string());
                }
            }
            PaletteTarget::Menu(action) => {
                self.post_palette_menu_action(wid, action);
            }
            PaletteTarget::Native(target) => {
                let _ = self.activate_native_palette_target(&target);
            }
            PaletteTarget::Theme(_) => {}
        }
    }

    /// Dispatch a native palette command if its window, view, generation, and enabled state still
    /// hold.
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

    fn palette_repaint_in(&mut self, wid: crate::WindowId) {
        self.sync_palette_pointer_cursor(wid);
        if let Some(ws) = self.windows.get_mut(&wid)
            && let Some(w) = &ws.os_window
        {
            w.request_redraw();
        }
        #[cfg(a11y_tree)]
        self.push_a11y_tree(wid);
    }

    /// Apply a [`SearchEdit`](crate::app_search::SearchEdit) to the open palette filter.
    #[cfg(test)]
    pub(crate) fn palette_edit(&mut self, edit: crate::app_search::SearchEdit) {
        if let Some(wid) = self.frontmost_window {
            self.palette_edit_in(wid, edit);
        }
    }

    fn palette_edit_in(&mut self, wid: crate::WindowId, edit: crate::app_search::SearchEdit) {
        if let Some(p) = self.windows.get_mut(&wid).and_then(|ws| ws.palette_mut()) {
            p.edit(edit);
        }
        self.palette_repaint_in(wid);
    }

    /// Execute a classified [`PaletteKey`] on the open palette.
    pub(crate) fn apply_palette_key_in(&mut self, wid: crate::WindowId, key: PaletteKey) {
        match key {
            PaletteKey::Close => self.palette_exit_in(wid),
            PaletteKey::Move(delta) => self.palette_move_in(wid, delta),
            PaletteKey::Activate => self.palette_activate_in(wid),
            PaletteKey::Edit(edit) => self.palette_edit_in(wid, edit),
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
        self.apply_palette_key_in(wid, palette_key(mods, ev));
        true
    }

    /// Handle an engine-neutral [`InputEvent`](crate::input::InputEvent) while the palette is open
    /// on `wid`.
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
            self.apply_palette_key_in(wid, key);
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

/// Classify an engine-neutral [`InputEvent`](crate::input::InputEvent) aimed at the open command
/// palette.
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
#[path = "app_palette_tests.rs"]
mod tests;
