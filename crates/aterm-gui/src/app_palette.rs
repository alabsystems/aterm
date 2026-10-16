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
//! Theme rows ([`PaletteTarget::Theme`](crate::palette::PaletteTarget)) preview
//! through the transient preview lane ([`crate::control_theme`]): the highlight is
//! read once per event-loop turn ([`App::sync_palette_theme_preview`]), so a held
//! arrow key or a pointer sweep costs one install per turn, not one per event.
//! Enter keeps the chosen theme standing while its `theme` write is in flight
//! ([`App::palette_choose_theme`]); Esc, or any other close, reverts.
//! Every mutator just `request_redraw`s (the state change rides
//! in `RepaintKey::settings_fp` via [`crate::WindowState::overlay_fp`]), and the frosted card
//! is built by the shared `splice_settings_panel`.

use crate::App;
use crate::Wake;
use crate::control_theme::PreviewHold;
use crate::palette::{
    NativeCommandScope, NativeCommandTarget, PaletteLive, PaletteState, PaletteTarget, PaletteTheme,
};

/// The `[keybindings]` face of a menu action, when one exists — the map that
/// lets [`App::palette_live`] resolve each menu row's accelerator HINT from the
/// EFFECTIVE binding table (platform seeds + the user's rebinds/unbinds) off
/// macOS, where the ⌘ key-equivalents are honestly blanked and the rows used to
/// show no chord at all. `None` = the command has no rebindable chord (its row
/// simply shows no hint). The pairing mirrors `dispatch_menu_action` ↔
/// `dispatch_action`: both faces of an entry here run the same verb, so the
/// hint can never advertise a chord that does something different.
#[cfg(not(target_os = "macos"))]
fn menu_binding(action: crate::menu::MenuAction) -> Option<crate::keybinding::Action> {
    use crate::keybinding::Action as K;
    use crate::menu::MenuAction as M;
    Some(match action {
        // Version is the glance, About the detail — both open the About route,
        // exactly as their dispatch arms do.
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
        // The Fabric menu's ledger row and the ledger key run the same method
        // (`App::open_session_ledger`), so the row shows the chord that fires it.
        M::LedgerForSession => K::OpenLedger,
        _ => return None,
    })
}

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
        state = state.with_themes(self.palette_themes());
        state.resolve(&self.palette_live());
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

    /// The palette's Theme section, from the admitted catalog only: every
    /// built-in (Default first, the registry's order), then each theme file that
    /// loaded, labelled by its own `name =` verbatim (its id joins the label when
    /// they differ, so two files sharing a name stay apart), its appearance the
    /// row's hint. A file that did not load has no row: there is nothing to draw.
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
            let label = if meta.display_name == id {
                id.to_string()
            } else {
                format!("{}  \u{b7}  {id}", meta.display_name)
            };
            themes.push(PaletteTheme {
                id: id.to_string(),
                label,
                appearance: meta.appearance,
                current: id.eq_ignore_ascii_case(current),
            });
        }
        themes
    }

    /// Open the command palette scoped to its Theme section (View ▸ Choose
    /// Theme…, the `choose_theme` binding), the cursor on the configured theme.
    /// An open command palette is closed first (reverting any preview it held),
    /// so the scoped one starts from what the config draws.
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

    /// Follow every open palette's theme highlight: a highlight that moved onto
    /// a theme previews it ([`PreviewHold::Palette`], no TTL); one that moved off
    /// the Theme section, or onto the configured theme, ends the palette's
    /// preview; and with no palette open anywhere a palette-held preview ends
    /// (a palette closed by another overlay, or with its window). Run once per
    /// event-loop turn (`about_to_wait`) and at a close, so a run of moves within
    /// one turn costs one install. Edge-triggered on each palette's highlight,
    /// so a `theme preview` from the control socket is not fought over.
    pub(crate) fn sync_palette_theme_preview(&mut self) {
        let front = self.frontmost_window;
        let mut any_open = false;
        let mut ask: Option<Option<String>> = None;
        let mut front_ask: Option<Option<String>> = None;
        for (&wid, ws) in &mut self.windows {
            let Some(palette) = ws.palette_mut() else {
                continue;
            };
            any_open = true;
            if let Some(change) = palette.take_theme_highlight_change() {
                if Some(wid) == front {
                    front_ask = Some(change);
                } else {
                    ask = Some(change);
                }
            }
        }
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
                    // The catalog moved under the open palette: draw what the
                    // config draws rather than a stale highlight.
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
    /// now: a plain name, the side of a `dark:…,light:…` pair, or the unset
    /// default's side. A name the catalog misses is returned as written, so
    /// choosing what it falls back to still writes.
    fn configured_side_name(&self) -> String {
        crate::app_config::resolve_theme_name_value(
            self.config.theme.as_deref(),
            self.os_appearance,
        )
        .unwrap_or_else(|| crate::app_config::unset_theme_name(self.os_appearance).to_string())
    }

    /// Enter on a Theme row: keep `id` on screen while its `theme` write is in
    /// flight ([`PreviewHold::Committing`]), so closing the palette does not
    /// flash the old theme back, then queue exactly one write. The theme the
    /// config already draws on this side writes nothing — a `dark:…,light:…`
    /// pair or an unset `theme` keeps following the OS — and only ends the
    /// palette's preview. A write that cannot be queued reverts the preview
    /// and says why.
    pub(crate) fn palette_choose_theme(&mut self, id: &str) {
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

    /// The live predicates that resolve per-row enabled/checked, read off the front window —
    /// the same conditions the native `validateMenuItem:` path uses (selection present,
    /// Settings open, full-screen, tab count) — plus the update-flow state the
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
        // The palette row is the cross-platform twin of the macOS Version-menu item,
        // so it carries the same apply-lane verdict: a row that offers an apply must
        // say when applying has already been tried and did not work.
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
        // The FRONT session's effective rain state (override else config bit);
        // false with no frontmost terminal — the row disables there anyway.
        let rain_on = self
            .frontmost_window
            .and_then(|wid| self.front_terminal(wid))
            .is_some_and(|terminal| self.session_rain_enabled(terminal.session));
        // The checkmark answers "is this window's promotable kitty (the
        // tenured program cat on glass, else the launch kitty) the pinned
        // favourite?" — the same look `App::favourite_kitty_checked` promotes. With
        // no window at all the launch kitty is the answer. `is_favourite` is
        // `&self`: this resolver cannot poll the startup import, and the
        // render loop polls every tick anyway.
        let promotable = self
            .frontmost_window
            .map_or(self.launch_kitty, |wid| self.promotable_kitty(wid));
        let kitty_favourited = self.kitty_log.is_favourite(promotable);
        // Per-session rows need a real front terminal — false over a native
        // whole tab AND with no window at all (the windowless-app state).
        let terminal_front = self
            .frontmost_window
            .is_some_and(|wid| self.front_terminal(wid).is_some());
        let can_rename = self
            .frontmost_window
            .is_some_and(|wid| self.can_rename_session(wid));
        // The front session's standing hold (round 19, the Fabric menu's halt
        // pair) — read from the session's own fabric leaf, the same fact the
        // presence band prints, so the palette and the band never disagree.
        let front_hold = self
            .frontmost_window
            .and_then(|wid| self.front_hold_fact(wid));
        // Accelerator hints for menu rows (keyboard audit #6): the resolved
        // chord for every menu action bound in the EFFECTIVE keybinding table,
        // deduped by action (a command listed in two menus shows one truth).
        // macOS supplies none — its rows keep their native ⌘ key-equivalents.
        #[cfg(target_os = "macos")]
        let menu_accels = Vec::new();
        #[cfg(not(target_os = "macos"))]
        let menu_accels = {
            let mut hints: Vec<(crate::menu::MenuAction, String)> = Vec::new();
            for section in crate::menu::MENU_MODEL {
                for entry in section.entries {
                    if let crate::menu::MenuEntry::Item { action, .. } = entry
                        && !hints.iter().any(|(a, _)| a == action)
                        && let Some(binding) = menu_binding(*action)
                        && let Some(chord) = self.keybindings.display_chord_for(binding)
                    {
                        hints.push((*action, chord));
                    }
                }
            }
            hints
        };
        PaletteLive {
            has_selection,
            can_rename,
            rain_on,
            kitty_favourited,
            serious_mode: self.serious_mode_enabled(),
            fullscreen,
            front_tab_split: self.front_tab_split(),
            multi_tab,
            native_tab_active,
            terminal_front,
            can_reopen_closed_tab: self.can_reopen_closed_tab(),
            can_reopen_closed_view: self.can_reopen_closed_view(),
            // A live capability question, not a platform one. It was
            // `cfg!(target_os = "macos")` while macOS owned the only picker;
            // Windows has a real `IFileOpenDialog` now, and both document
            // runtimes already worked here — so the rows come alive from the
            // same predicate Settings ▸ Wallpaper's button asks, and stay grey
            // wherever the dialog genuinely cannot open. HEADLESS is one such
            // place, folded here rather than in the platform predicate: a
            // headless instance has no glass to hang the panel on and no
            // application object to present it from (on macOS its display-free
            // loop builds no `NSApplication`) — `invoke OpenMarkdown` must
            // answer "disabled", never park the main thread in a modal nothing
            // can dismiss.
            local_file_picker_available: !self.headless
                && crate::menu::local_file_picker_available(),
            front_hold,
            presence_band: self.presence_band_on(),
            presence_rim: self.presence_rim_on(),
            staged,
            staged_trouble,
            staged_on_hold,
            realized,
            // Reduced motion pins the realized celebration fade at full alpha.
            // Serious mode removes the decorative row altogether above.
            // Focused=true: the palette only paints on the focused front window.
            reduced_motion: self.motion_policy(true) == crate::motion::MotionPolicy::Reduced,
            menu_accels,
        }
    }

    /// Whether the front window's active tab is split, so ⌘W closes a pane
    /// rather than the tab (`close_active_tab`).
    fn front_tab_split(&self) -> bool {
        self.front()
            .and_then(|ws| ws.tab_set.active())
            .is_some_and(|tab| tab.root.len() > 1)
    }

    /// Publish the native bar's live bits ([`crate::menu::MenuLive`]) — the facts
    /// [`Self::palette_live`] reads for the same rows — for `validateMenuItem:`,
    /// which AppKit calls synchronously when a menu opens and which cannot reach
    /// `App`. Called on the way to every wait (macOS, windowed), so it allocates
    /// nothing and never waits on a terminal: the selection is read with a
    /// try-lock, and a busy terminal keeps the last bit (a stale Copy bit costs no
    /// keystroke — a disabled row's ⌘C falls through to `on_key`, which copies).
    /// Over a native tab Copy stays enabled: reading its selection would copy
    /// the selected text on every wait. While a rename field is open Copy
    /// stays enabled too, so ⌘C reaches `divert_menu_action_around_rename`: a
    /// native field's editor gets `copy:` (it has no ⌘C of its own); the
    /// in-grid field claims it inert.
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
            multi_tab: self.front().is_some_and(|ws| ws.tab_set.len() >= 2),
            can_reopen_closed_tab: self.can_reopen_closed_tab(),
            can_reopen_closed_view: self.can_reopen_closed_view(),
            serious_mode: self.serious_mode_enabled(),
            rain_on: terminal.is_some_and(|t| self.session_rain_enabled(t.session)),
            kitty_favourited: self.kitty_log.is_favourite(promotable),
            front_tab_split: self.front_tab_split(),
        });
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
        self.palette_open_state(wid, state);
    }

    /// Put `state` up as the front window's palette (no-op if one is open).
    fn palette_open_state(&mut self, wid: crate::WindowId, state: PaletteState) {
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
        // Refresh the accessibility tree so a screen reader sees the open palette.
        self.overlay_a11y_update();
    }

    /// Surface the exact recovery capabilities returned by a blocked native
    /// close. The caller has already focused `view`; this method preserves that
    /// stable target and opens a recovery-only palette in the owning window so
    /// Cmd-W, tab-close, control, window-close, and quit never look inert.
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
        state.resolve(&self.palette_live());

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
            ws.scroll_residual_x = 0.0;
            if let Some(w) = &ws.os_window {
                w.request_redraw();
            }
            closed = true;
        }
        // Publish the now-empty tree (the overlay closed).
        self.overlay_a11y_update();
        // Closing reverts what the palette's highlight was previewing (a theme
        // chosen with Enter is held as Committing, which this leaves standing).
        if closed {
            self.end_theme_preview_if(|preview| preview.hold == PreviewHold::Palette);
        }
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
        if let PaletteTarget::Theme(id) = target {
            // Held as Committing before the close, so the close keeps it.
            self.palette_choose_theme(&id);
            self.palette_exit();
            return;
        }
        self.palette_exit();
        match target {
            PaletteTarget::Menu(action) => {
                if let Some(proxy) = self.proxy.as_ref() {
                    let _ = proxy.send_event(Wake::MenuAction { action });
                }
            }
            PaletteTarget::Native(target) => {
                let _ = self.activate_native_palette_target(&target);
            }
            PaletteTarget::Theme(_) => {}
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
    /// activates, Esc closes. Closed ⇒ `false` (keys flow normally).
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
    /// `key`/`text` verbs (introspection CONTROL of the overlay). The caller still
    /// swallows the event from the PTY.
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
        // THE KEYPAD IS ITS MAIN-BLOCK TWIN ON AN OVERLAY, as it is on a native
        // page and in the seam's press classifier. This card reads the key for
        // what it MEANS, and `keymap::build_key_input` hands it the keypad
        // identity (`Numpad5`, `NumpadEnd`) so the PTY encoders can tell KP_5
        // from 5 — which the arms below have no case for: a keypad digit
        // typed NOTHING into the filter, where the main row's types its glyph.
        // `InputEvent::keypad_folded` is the one fold, shared with the native
        // pages, and a controller's `key kp5` takes the same road.
        let folded = ev.keypad_folded();
        let ev = folded.as_ref().unwrap_or(ev);
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
            InputEvent::Text(t) | InputEvent::Paste(t, _) => {
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

    /// THE MENU BAR PUBLISHES WHAT THE PALETTE RESOLVES. `publish_menu_live` —
    /// what the native bar's `validateMenuItem:` reads — derives each bit from
    /// the same fact `palette_live` does: one tab greys the tab cycle, no
    /// selection greys Copy, serious mode checks its row, and a split front tab
    /// titles ⌘W's row Close Pane (on the bar and in the palette).
    #[test]
    fn the_menu_bar_publishes_what_the_palette_resolves() {
        use crate::menu::{MenuAction, menu_live, native_live_title, set_menu_live};
        let _statics = crate::menu::MENU_STATICS
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let before = menu_live();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.frontmost_window = Some(wid);
        let agrees = |app: &App| {
            app.publish_menu_live();
            let bar = menu_live();
            let palette = app.palette_live();
            assert_eq!(bar.has_selection, palette.has_selection);
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

    /// An open rename keeps the Copy row enabled so ⌘C reaches the divert
    /// (`divert_menu_action_around_rename`) instead of the field editor (which,
    /// for a native field, has no ⌘C of its own). Headless, the in-grid field
    /// presents the edit, and the divert claims its Copy inert.
    #[test]
    fn a_live_rename_keeps_the_menu_bar_copy_row_enabled() {
        use crate::menu::{menu_live, set_menu_live};
        let _statics = crate::menu::MENU_STATICS
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let before = menu_live();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.frontmost_window = Some(wid);
        app.tab_strip_rows = 1; // the in-grid strip presents the editor headless
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

    /// A ⌘ chord is a command, never text. A menu row the bar greys hands its
    /// key equivalent to `on_key` — ⌘C with no selection, ⇧⌘] with one tab,
    /// ⇧⌘T with nothing to reopen — and with the palette open those typed `c`,
    /// `}` and `T` into its search.
    #[test]
    fn a_command_chord_never_types_into_the_palette_search() {
        use winit::event::{ElementState, KeyEvent};
        use winit::keyboard::{Key, KeyCode, KeyLocation, ModifiersState, PhysicalKey, SmolStr};
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
        // Non-vacuity: the same key with no ⌘ types.
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

    /// END TO END through the real `App`: the window's live keybinding table
    /// reaches the painted card AND the `controls menu` introspection lines,
    /// which are the same rows a screen reader is handed. Off macOS this is the
    /// only way to learn a chord without leaving the app.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn palette_rows_carry_the_live_chord() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        // The harness constructs with an EMPTY table; a real launch installs
        // `Keybindings::resolved(config)` — install exactly that.
        app.keybindings = crate::keybinding::Keybindings::resolved(None);
        let accel_of = |app: &App, label: &str| {
            app.palette_snapshot(wid)
                .controls_lines()
                .into_iter()
                .find(|line| line.contains(&format!("label={label:?}")))
                .unwrap_or_else(|| panic!("{label} row"))
        };
        assert!(
            accel_of(&app, "Enter Full Screen").contains(r#"accel="F11""#),
            "{}",
            accel_of(&app, "Enter Full Screen"),
        );
        assert!(
            accel_of(&app, "New Terminal Tab").contains(r#"accel="Ctrl+Shift+T""#),
            "{}",
            accel_of(&app, "New Terminal Tab"),
        );
        // A user rebind is what the running app displays — the thing a static
        // menu bar's key-equivalents structurally cannot do.
        let mut table = std::collections::BTreeMap::new();
        table.insert("ctrl+alt+n".to_string(), "new_window".to_string());
        app.keybindings = crate::keybinding::Keybindings::resolved(Some(&table));
        assert!(
            accel_of(&app, "New Window").contains(r#"accel="Ctrl+Shift+N""#),
            "the seeded chord still fires it, so it is still the label: {}",
            accel_of(&app, "New Window"),
        );
        let mut table = std::collections::BTreeMap::new();
        table.insert("ctrl+shift+n".to_string(), "rename_session".to_string());
        table.insert("ctrl+alt+n".to_string(), "new_window".to_string());
        app.keybindings = crate::keybinding::Keybindings::resolved(Some(&table));
        assert!(
            accel_of(&app, "New Window").contains(r#"accel="Ctrl+Alt+N""#),
            "with the default rebound away, the user's own chord shows: {}",
            accel_of(&app, "New Window"),
        );
    }

    /// The join covers the find-again rows too: `find_next`/`find_prev` ship
    /// UNSEEDED (F3 is owed to the program on the other side of the PTY), so
    /// their rows are blank by default — and the moment a user binds them, the
    /// palette says so. This is the new-vocabulary half of the F11 work: a
    /// command that used to be menu-only now round-trips config → chord → row.
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
        // The advertised chord is PLATFORM-TRUE (audit I9): ⌘F on macOS, the
        // seeded Ctrl+Shift+F off it — never a ⌘ glyph on a keyboard without one.
        let accel = if cfg!(target_os = "macos") {
            "accel=\"Cmd-F\""
        } else {
            "accel=\"Ctrl-Shift-F\""
        };
        assert!(closed.iter().any(|line| {
            line.contains("section=\"Settings App\"")
                && line.contains("action=settings/search")
                && line.contains(accel)
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

    /// A headless instance has no glass to hang a file picker on — and no
    /// application object to present the `NSOpenPanel` from — so the two
    /// local-file rows read disabled there, and
    /// `invoke OpenMarkdown` answers "disabled" instead of parking the main
    /// thread in a modal nothing can dismiss. Windowed, the platform predicate
    /// decides as before.
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

    /// A theme change that is NOT an OS appearance flip — a colour-only config
    /// reload, a theme-catalog reload, or a program's OSC 11 background — reaches
    /// an OPEN overlay card: the palette's model fingerprint and the geometry are
    /// unchanged, so the card cache must key on the theme it paints with, or it
    /// keeps compositing the previous colours until the next keystroke.
    #[test]
    fn open_palette_card_re_rasterizes_on_a_live_theme_swap() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        // The terminal overlay sizes itself against the composed frame's rows.
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

        // The reload path: `apply_theme_live` with the OS appearance untouched.
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

        // The OSC 11 path: the live default background the card tints off.
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

    // ---- The Theme section: live preview, Enter keeps, Esc reverts ----------

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
        let at = {
            let palette = front_palette(app);
            palette
                .filtered()
                .iter()
                .position(|&i| palette.rows()[i].action == PaletteTarget::Theme(id.to_string()))
                .unwrap_or_else(|| panic!("{id} is a row"))
        };
        let selected = {
            let palette = front_palette(app);
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
        // The appearance is searchable too: every theme row "light" keeps is light.
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
        // A held key's repeats inside one turn: nothing is installed until the
        // turn's sync, and then once.
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
        // A second sync with the highlight unmoved changes nothing.
        app.sync_palette_theme_preview();
        assert_eq!(app.engine_color_fanouts, fanouts + 1);
        assert_eq!(durable(&app), durable_before, "a preview writes nothing");

        // Moving to the other row previews it.
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

    /// A palette closed some other way (another overlay took the slot) reverts
    /// at the next turn's sync; a highlight that leaves the Theme section
    /// reverts at once.
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
            app.palette_backspace(); // the cursor returns to the first (menu) row
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
                front_palette(&app)
                    .selected_target()
                    .is_some_and(|target| !matches!(target, PaletteTarget::Theme(_)))
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

    /// View ▸ Choose Theme… (and the `choose_theme` key) opens the palette
    /// scoped to its Theme section, the cursor on the configured theme, which
    /// is not re-installed; its rows are exactly the theme rows.
    #[test]
    fn choose_theme_opens_the_scoped_palette_on_the_configured_theme() {
        let mut app = themed_app("theme = \"Nord\"\n");
        let fanouts = app.engine_color_fanouts;
        app.dispatch_action(WindowId(0), crate::keybinding::Action::ChooseTheme);
        let palette = front_palette(&app);
        assert!(palette.is_themes_only());
        assert_eq!(palette.highlighted_theme(), Some("Nord"));
        // Every theme row, then the section's way to Settings ▸ Themes.
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
        // Opening it again is a no-op, not a toggle.
        app.open_theme_palette();
        assert!(front_palette(&app).is_themes_only());
        assert_eq!(
            crate::keybinding::Action::parse("choose_theme"),
            Some(crate::keybinding::Action::ChooseTheme)
        );
        assert!(crate::keybinding::ACTION_NAMES.contains(&"choose_theme"));
    }
}
