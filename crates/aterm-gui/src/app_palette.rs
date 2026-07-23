// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `App` glue for the in-window command PALETTE ([`crate::palette`]): open/close (the
//! `Action::OpenPalette` keybinding, the macOS View ▸ Command Palette… menu item, or the
//! `menu` control verb), type-to-filter, move the cursor, and ACTIVATE the selected command.
//!
//! Menu activation posts [`Wake::MenuAction`](crate::Wake) — the exact relay the native
//! `MenuTarget::menuAction:` uses ([`crate::menu`]). Active native-tab commands share the
//! surface but carry an exact window/view/generation target and enter their reducer as
//! `AppEvent::Action` only while that identity remains current.
//! Modelled on `app_about.rs`; every mutator just `request_redraw`s (the state change rides
//! in `RepaintKey::settings_fp` via [`crate::WindowState::overlay_fp`]), and the frosted card
//! is built by the shared `splice_settings_panel`.

use crate::App;
use crate::Wake;
use crate::palette::{NativeCommandScope, NativeCommandTarget, PaletteLive, PaletteState};

impl App {
    /// Snapshot command metadata for exactly the active native view in `wid`. A terminal
    /// tab returns `None`, so app-local rows structurally disappear from its palette.
    fn native_palette_scope(&self, wid: crate::WindowId) -> Option<NativeCommandScope> {
        let (instance, view) = self.active_native_view(wid)?;
        let generation = self.native_runtime.view_generation(view)?;
        let app = self.native_runtime.app(instance)?;
        let commands = self.native_runtime.commands(instance, view).ok()?;
        Some(NativeCommandScope {
            window: wid,
            instance,
            view,
            generation,
            section: format!("{} App", app.descriptor().name),
            commands,
        })
    }

    /// Build one resolved palette snapshot for a window. Controls use this same constructor
    /// when the palette is closed, preserving screen/introspection row parity.
    pub(crate) fn palette_snapshot(&self, wid: crate::WindowId) -> PaletteState {
        let mut state = PaletteState::new();
        if let Some(scope) = self.native_palette_scope(wid) {
            state = state.with_native_commands(scope);
        }
        state.resolve(&self.palette_live());
        state
    }

    /// The live predicates that resolve per-row enabled/checked, read off the front window —
    /// the same conditions the native `validateMenuItem:` path uses (selection present, HUD
    /// bands on, Settings open, full-screen, tab count) — plus the update-flow state the
    /// Version section's dynamic row mirrors (staged nudge / post-update realized arrow).
    /// `pub(crate)`: the `controls menu` closed-palette fallback (`app_introspect`)
    /// resolves a fresh snapshot against the same predicates.
    pub(crate) fn palette_live(&self) -> PaletteLive {
        let has_selection = self.frontmost_window.is_some_and(|wid| {
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
        let fullscreen = self
            .front()
            .and_then(|ws| ws.os_window.as_ref())
            .and_then(|w| w.fullscreen())
            .is_some();
        let multi_tab = self.front().is_some_and(|ws| ws.tab_set.len() >= 2);
        let native_tab_active = self
            .frontmost_window
            .is_some_and(|wid| self.active_native_view(wid).is_some());
        // The staged nudge state (set only for a strictly-newer build — the
        // `Wake::UpdateStaged` contract) drives the one-click row; the realized arrow
        // (post-update boot, TTL-bounded by the about_to_wait sweep) drives the fading
        // celebration row. Both may be live at once — `resolve` gives staged precedence.
        let staged = self.relaunch.as_ref().map(|r| (r.build, r.version.clone()));
        let realized = if self.serious_mode_enabled() {
            None
        } else {
            self.upgrade_realized
                .filter(|t| t.elapsed() < crate::relaunch_notice::REALIZED_ARROW_TTL)
                .map(|t| (crate::build_info::version_display().to_string(), t))
        };
        // The FRONT session's effective rain state (override else config bit);
        // false with no frontmost terminal — the row disables there anyway.
        let rain_on = self
            .frontmost_window
            .and_then(|wid| self.front_terminal(wid))
            .is_some_and(|terminal| self.session_rain_enabled(terminal.session));
        // Per-session rows need a real front terminal — false over a native
        // whole tab AND with no window at all (the windowless-app state).
        let terminal_front = self
            .frontmost_window
            .is_some_and(|wid| self.front_terminal(wid).is_some());
        PaletteLive {
            has_selection,
            hud_master: self.config.show_hud_or_default(),
            resources_hud: self.panel_enabled(crate::hud_bar::PanelId::Resources),
            engine_hud: self.panel_enabled(crate::hud_bar::PanelId::Engine),
            settings_open: self.settings_tab_open(),
            rain_on,
            serious_mode: self.serious_mode_enabled(),
            fullscreen,
            multi_tab,
            native_tab_active,
            terminal_front,
            can_reopen_closed_tab: self.can_reopen_closed_tab(),
            can_reopen_closed_view: self.can_reopen_closed_view(),
            local_file_picker_available: cfg!(target_os = "macos"),
            staged,
            realized,
            // Reduced motion pins the realized celebration fade at full alpha.
            // Serious mode removes the decorative row altogether above.
            // Focused=true: the palette only paints on the focused front window.
            reduced_motion: self.motion_policy(true) == crate::motion::MotionPolicy::Reduced,
        }
    }

    /// Re-resolve every OPEN palette against the live predicates and repaint — called on
    /// update-flow transitions (`Wake::UpdateStaged` lands, the realized arrow spawns or
    /// its TTL expires) so an open palette's Version rows update IN PLACE; without this
    /// it would keep pre-transition rows until closed and reopened.
    pub(crate) fn palette_refresh_live(&mut self) {
        let live = self.palette_live();
        let mut touched = Vec::new();
        for (&wid, ws) in &mut self.windows {
            if let Some(p) = ws.palette_mut() {
                p.resolve(&live);
                if let Some(w) = &ws.os_window {
                    w.request_redraw();
                }
                touched.push(wid);
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

    /// Re-scope an already-open palette after the canonical active tab changes. A terminal
    /// removes every native row; a native app contributes fresh commands stamped with its
    /// own view generation. The query survives, while selection returns to the first match.
    pub(crate) fn palette_sync_native_scope(&mut self, wid: crate::WindowId) {
        let scope = self.native_palette_scope(wid);
        let live = self.palette_live();
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

    /// Open the command palette on the front window. Settings, About, and the palette are
    /// MUTUALLY EXCLUSIVE modal overlays (they share the one card slot), so this closes the
    /// others first. No-op if the palette is already open.
    pub(crate) fn palette_enter(&mut self) {
        let Some(wid) = self.frontmost_window else {
            return;
        };
        let state = self.palette_snapshot(wid);
        let mut opened = false;
        if let Some(ws) = self.front_mut()
            && ws.palette().is_none()
        {
            // Mutual exclusion is STRUCTURAL now: assigning the one `overlay` slot drops
            // whatever Settings/About was there — no manual "clear the other two".
            ws.overlay = Some(crate::overlay::Overlay::Palette(state));
            // Wheel fractions belong to one surface only. Drop any terminal/native
            // remainder before the palette begins consuming the shared device stream.
            ws.scroll_residual = 0.0;
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
        // Refresh the accessibility tree so a screen reader sees the open palette.
        self.overlay_a11y_update();
    }

    /// Close the command palette on the front window (no-op if already closed).
    pub(crate) fn palette_exit(&mut self) {
        let wid = self.frontmost_window;
        let mut closed = false;
        if let Some(ws) = self.front_mut()
            && ws.palette().is_some()
        {
            ws.overlay = None;
            // A sub-line palette gesture must never drain into terminal/native scrolling
            // after the modal closes.
            ws.scroll_residual = 0.0;
            if let Some(w) = &ws.os_window {
                w.request_redraw();
            }
            closed = true;
        }
        // Publish the now-empty tree (the overlay closed).
        self.overlay_a11y_update();
        if closed
            && let Some(wid) = wid
            && let Some((x, y)) = self.windows.get(&wid).map(|window| window.last_cursor_px)
        {
            // Re-resolve the cursor immediately against the revealed surface. Keyboard
            // close must not leave a stale neutral pointer over editor text until motion.
            self.on_cursor_moved(wid, x, y);
        }
    }

    /// Toggle the command palette (the keybinding + the macOS menu item land here).
    pub(crate) fn toggle_palette(&mut self) {
        if self.front().is_some_and(|ws| ws.palette().is_some()) {
            self.palette_exit();
        } else {
            self.palette_enter();
        }
    }

    /// Move the palette cursor by `delta` (Up/Down) over the filtered set.
    pub(crate) fn palette_move(&mut self, delta: isize) {
        if let Some(p) = self.front_mut().and_then(|ws| ws.palette_mut()) {
            p.move_selection(delta);
        }
        self.palette_repaint_front();
    }

    /// Append a filter character and re-narrow the list.
    pub(crate) fn palette_filter_push(&mut self, c: char) {
        if let Some(p) = self.front_mut().and_then(|ws| ws.palette_mut()) {
            p.push_char(c);
        }
        self.palette_repaint_front();
    }

    /// Delete the last filter character (Backspace).
    pub(crate) fn palette_backspace(&mut self) {
        if let Some(p) = self.front_mut().and_then(|ws| ws.palette_mut()) {
            p.backspace();
        }
        self.palette_repaint_front();
    }

    /// Activate the selected typed target. Menu rows retain the native-menu relay. Native
    /// rows synchronously revalidate exact lifecycle identity and current enabled metadata;
    /// a tab switch, detach, generation advance, or disabled command makes activation inert.
    pub(crate) fn palette_activate(&mut self) {
        let Some(target) = self
            .front()
            .and_then(|ws| ws.palette())
            .and_then(PaletteState::selected_target)
        else {
            return;
        };
        self.palette_exit();
        match target {
            crate::palette::PaletteTarget::Menu(action) => {
                if let Some(proxy) = self.proxy.as_ref() {
                    let _ = proxy.send_event(Wake::MenuAction { action });
                }
            }
            crate::palette::PaletteTarget::Native(target) => {
                let _ = self.activate_native_palette_target(&target);
            }
        }
    }

    /// Generation-safe native-command sink, separated for focused positive and stale-target
    /// negative tests. Re-reading commands here is the final enabled-state authority; an
    /// open palette can outlive an async reducer update, so its painted snapshot is not
    /// trusted for dispatch.
    fn activate_native_palette_target(
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
        // Typing/moving re-narrows or re-selects: refresh the accessibility tree too, so the
        // screen-reader view of the filtered rows + focus tracks the on-screen list.
        self.overlay_a11y_update();
    }

    /// While the palette is open on `wid`, drive it from the keyboard and SWALLOW every key
    /// (return `true`): printable chars filter, Backspace deletes, Up/Down move, Enter
    /// activates, Esc closes. Closed ⇒ `false` (keys flow normally). Mirrors
    /// `on_key_about_mode`, checked just before it in `on_key`.
    pub(crate) fn on_key_palette_mode(
        &mut self,
        wid: crate::WindowId,
        ev: &winit::event::KeyEvent,
    ) -> bool {
        use winit::keyboard::{Key, NamedKey};
        if self.windows.get(&wid).and_then(|ws| ws.palette()).is_none() {
            return false;
        }
        match &ev.logical_key {
            Key::Named(NamedKey::Escape) => self.palette_exit(),
            Key::Named(NamedKey::ArrowUp) => self.palette_move(-1),
            Key::Named(NamedKey::ArrowDown) => self.palette_move(1),
            Key::Named(NamedKey::Enter) => self.palette_activate(),
            Key::Named(NamedKey::Backspace) => self.palette_backspace(),
            Key::Named(NamedKey::Space) => self.palette_filter_push(' '),
            Key::Character(s) => {
                for c in s.chars().filter(|c| !c.is_control()) {
                    self.palette_filter_push(c);
                }
            }
            _ => {
                if let Some(t) = ev.text.as_deref() {
                    for c in t.chars().filter(|c| !c.is_control()) {
                        self.palette_filter_push(c);
                    }
                }
            }
        }
        true
    }

    /// The ENGINE-NEUTRAL twin of [`Self::on_key_palette_mode`] — reached by controller
    /// `key`/`text` verbs (introspection CONTROL of the overlay), mirroring
    /// `about_input_event`. The caller still swallows the event from the PTY.
    pub(crate) fn palette_input_event(
        &mut self,
        wid: crate::WindowId,
        ev: &crate::input::InputEvent,
    ) {
        use crate::input::InputEvent;
        use aterm_types::keyboard::{Key as TKey, KeyEventType, NamedKey as TNamed};
        if self.windows.get(&wid).and_then(|ws| ws.palette()).is_none() {
            return;
        }
        match ev {
            InputEvent::Key {
                key, event_type, ..
            } => {
                if matches!(event_type, KeyEventType::Release) {
                    return;
                }
                match key {
                    TKey::Named(TNamed::Escape) => self.palette_exit(),
                    TKey::Named(TNamed::ArrowUp) => self.palette_move(-1),
                    TKey::Named(TNamed::ArrowDown) => self.palette_move(1),
                    TKey::Named(TNamed::Enter) => self.palette_activate(),
                    TKey::Named(TNamed::Backspace) => self.palette_backspace(),
                    TKey::Named(TNamed::Space) => self.palette_filter_push(' '),
                    TKey::Character(c) if !c.is_control() => self.palette_filter_push(*c),
                    _ => {}
                }
            }
            InputEvent::Text(t) | InputEvent::Paste(t) => {
                for c in t.chars().filter(|c| !c.is_control()) {
                    self.palette_filter_push(c);
                }
            }
            _ => {}
        }
    }
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
    fn toggle_opens_and_closes_palette() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.windows.get(&wid).unwrap().palette().is_none());
        app.toggle_palette();
        assert!(app.windows.get(&wid).unwrap().palette().is_some(), "opened");
        app.toggle_palette();
        assert!(app.windows.get(&wid).unwrap().palette().is_none(), "closed");
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
                .any(|line| line.contains("Updated to v"))
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
    fn palette_is_mutually_exclusive_with_settings_and_about() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.settings_enter();
        app.about_enter();
        assert!(app.windows.get(&wid).unwrap().about().is_some());
        app.palette_enter();
        let ws = app.windows.get(&wid).unwrap();
        assert!(ws.palette().is_some(), "palette open");
        assert!(ws.settings().is_none(), "settings closed");
        assert!(ws.about().is_none(), "about closed");
    }

    #[test]
    fn each_overlay_enter_closes_the_other_two() {
        // The three overlays share ONE card slot and must be mutually exclusive in
        // BOTH directions. Regression: about_enter/settings_enter used to leave a
        // live palette, which then swallowed every key (the on_key gate checks
        // palette first) UNDER the shown card — visible card, hidden controller.
        let wid = WindowId(0);

        // Palette open first, then About → About wins, palette must close.
        let mut app = App::headless_for_test();
        app.palette_enter();
        app.about_enter();
        let ws = app.windows.get(&wid).unwrap();
        assert!(ws.about().is_some(), "about open");
        assert!(ws.palette().is_none(), "about_enter must close the palette");
        assert!(ws.settings().is_none(), "settings closed");

        // Palette open first, then Settings → Settings wins, palette+about close.
        let mut app = App::headless_for_test();
        app.about_enter();
        app.palette_enter();
        app.settings_enter();
        let ws = app.windows.get(&wid).unwrap();
        assert!(ws.settings().is_some(), "settings open");
        assert!(
            ws.palette().is_none(),
            "settings_enter must close the palette"
        );
        assert!(ws.about().is_none(), "settings_enter must close about");
    }

    #[test]
    fn filter_then_activate_posts_the_action() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.palette_enter();
        for c in "help".chars() {
            app.palette_filter_push(c);
        }
        // The Help command is the survivor; activating closes the palette (and, with a
        // proxy, would post Wake::MenuAction — none in the headless test harness).
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
        filter(&mut app, "Settings: Search");
        let target = app
            .windows
            .get(&wid)
            .and_then(|window| window.palette())
            .and_then(crate::palette::PaletteState::selected_target)
            .expect("native search command");
        assert!(matches!(
            &target,
            crate::palette::PaletteTarget::Native(target)
                if target.window == wid
                    && target.instance == instance
                    && target.view == view
                    && target.generation == generation
                    && target.action.as_str() == "settings/search"
        ));

        app.palette_activate();
        assert!(app.windows.get(&wid).unwrap().palette().is_none());
        let focus = app
            .native_runtime
            .view_state(view)
            .and_then(|state| state.common().last_focus.as_ref())
            .map(crate::native_ui::UiKey::as_str);
        assert_eq!(focus, Some("settings/search"));
    }

    #[test]
    fn stale_native_palette_target_cannot_redirect_after_tab_switch() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (_, settings_view) = app.active_native_view(wid).expect("Settings active");
        let before_focus = app
            .native_runtime
            .view_state(settings_view)
            .and_then(|state| state.common().last_focus.clone());
        app.palette_enter();
        filter(&mut app, "Settings: Search");
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
        let after_focus = app
            .native_runtime
            .view_state(settings_view)
            .and_then(|state| state.common().last_focus.clone());
        assert_eq!(
            after_focus, before_focus,
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
                && line.contains("action=settings/search")
                && line.contains("accel=\"Cmd-F\"")
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
}
