// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native Settings-tab host glue: singleton view discovery, open/focus/close, stable
//! route navigation, and compatibility control projections. The lower half retains
//! the former overlay-input adapter as test scaffolding around [`crate::settings`];
//! production cannot construct its `Overlay::Settings` variant.

#![allow(
    dead_code,
    reason = "legacy Settings overlay input remains test scaffolding; production routes through the native tab methods in this module"
)]

use crate::{App, Wake, WindowState};

/// The §L.3 anonymous suggestion form (Google Forms): the landing page's Send
/// opens it PREFILLED in the default browser — the overlay itself never talks
/// to the network, and the form collects no respondent identity.
const SUGGEST_FORM_URL: &str = "https://docs.google.com/forms/d/e/1FAIpQLScet_59v_RHdQ3PtyKFSb95jmg87dOyiJSvlhFnomEC3atE2A/viewform";
/// The form's one paragraph question ("What should our next aterm update be?").
const SUGGEST_FORM_FIELD: &str = "entry.2053788710";

/// Minimal RFC 3986 query-value percent-encoder for the prefill URL: unreserved
/// bytes pass, everything else (incl. UTF-8 continuation bytes) becomes `%XX`.
/// Std-only and bounded by the input length — no crate for one query value.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len().saturating_mul(3));
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push('%');
                out.push(
                    char::from_digit(u32::from(b >> 4), 16)
                        .unwrap_or('0')
                        .to_ascii_uppercase(),
                );
                out.push(
                    char::from_digit(u32::from(b & 0xF), 16)
                        .unwrap_or('0')
                        .to_ascii_uppercase(),
                );
            }
        }
    }
    out
}

impl App {
    /// Find the Settings presentation in `wid` by stable native-app identity.
    ///
    /// Settings is a process-singleton controller with zero or more tab views. It is
    /// never represented by a special OS window or a sentinel terminal session.
    fn settings_tab_in_window(
        &self,
        wid: crate::WindowId,
    ) -> Option<(
        usize,
        crate::tab_model::TabId,
        crate::native_app::AppInstanceId,
        crate::tab_model::ViewId,
    )> {
        self.windows
            .get(&wid)?
            .tab_set
            .tabs()
            .iter()
            .enumerate()
            .find_map(|(index, tab)| {
                let view = tab.focus;
                let crate::tab_model::View::Native(native) = self.view_store.get(view).copied()?
                else {
                    return None;
                };
                (self.native_runtime.app(native.instance)?.kind()
                    == crate::native_app::AppKind::Settings)
                    .then_some((index, tab.id, native.instance, view))
            })
    }

    /// Whether any window presents the process-singleton native Settings app.
    pub(crate) fn settings_tab_open(&self) -> bool {
        self.windows
            .keys()
            .copied()
            .any(|wid| self.settings_tab_in_window(wid).is_some())
    }

    /// Publish the process-global Smart Titles runtime observation into every
    /// open Settings view. Configuration remains the durable source of intent;
    /// this is the live provider/locality/readiness/error projection. The update
    /// is equality-gated and only redraws windows whose view actually changed.
    pub(crate) fn sync_settings_title_summary_health(&mut self) {
        let health = self.title_summary_health();
        let targets = self
            .windows
            .keys()
            .copied()
            .filter_map(|wid| {
                self.settings_tab_in_window(wid)
                    .map(|(_, _, _, view)| (wid, view))
            })
            .collect::<Vec<_>>();
        for (wid, view) in targets {
            let changed = match self.native_runtime.view_state_mut(view) {
                Some(crate::native_app::AppViewState::Settings(state)) => {
                    state.replace_title_summary_health(health.clone())
                }
                Some(
                    crate::native_app::AppViewState::Markdown(_)
                    | crate::native_app::AppViewState::Editor(_)
                    | crate::native_app::AppViewState::Recovery(_),
                )
                | None => false,
            };
            if changed && let Some(window) = self.windows.get_mut(&wid) {
                window.last_present = None;
                if let Some(os_window) = window.os_window.as_ref() {
                    os_window.request_redraw();
                }
            }
        }
    }

    /// Legacy SettingsState projection for compatibility control verbs. The
    /// authoritative surface is still the native tab; this returns the exact data
    /// model embedded in one of its live per-view controllers.
    pub(crate) fn native_settings_legacy_state(&self) -> Option<&crate::settings::SettingsState> {
        let front = self
            .frontmost_window
            .and_then(|wid| self.settings_tab_in_window(wid).map(|(_, _, _, view)| view));
        let view = front.or_else(|| {
            self.windows
                .keys()
                .copied()
                .find_map(|wid| self.settings_tab_in_window(wid).map(|(_, _, _, view)| view))
        })?;
        match self.native_runtime.view_state(view)? {
            crate::native_app::AppViewState::Settings(state) => Some(&state.legacy),
            crate::native_app::AppViewState::Markdown(_)
            | crate::native_app::AppViewState::Editor(_)
            | crate::native_app::AppViewState::Recovery(_) => None,
        }
    }

    /// Host for the retired Settings overlay used by legacy model tests. Production
    /// Settings is a native tab and never enters the window overlay slot.
    fn settings_host(&self) -> Option<&WindowState> {
        self.frontmost_window.and_then(|wid| self.windows.get(&wid))
    }

    /// Mutable twin of [`Self::settings_host`].
    fn settings_host_mut(&mut self) -> Option<&mut WindowState> {
        let wid = self.frontmost_window?;
        self.windows.get_mut(&wid)
    }

    /// Refresh the open overlay's Kitty Log SNAPSHOT (§F4.6) from the App's
    /// in-memory log when it is stale — the snapshot discipline that keeps
    /// `settings_tray` a pure painter. Called on overlay open, on category
    /// switches, at the start of every redraw (the drain-while-open path: a
    /// sighting bumps the host revision, the next frame syncs + repaints),
    /// and before `controls prefs` serializes. Cheap at rest: one revision
    /// compare, no clone. The repaint rides `RepaintKey::settings_fp`, which
    /// folds the revision only while the Kitty Log category is active.
    pub(crate) fn sync_settings_kitty_log(&mut self) {
        let rev = self.kitty_log.revision();
        let stale = self
            .settings_host()
            .and_then(|ws| ws.settings())
            .is_some_and(|s| s.kitty_log.revision != rev);
        if !stale {
            return;
        }
        let view = self.kitty_log.view();
        if let Some(ws) = self.settings_host_mut() {
            let on_page = ws
                .settings_mut()
                .map(|s| {
                    *s.kitty_log = view;
                    s.category == crate::prefs::Section::KittyLog
                })
                .unwrap_or(false);
            // Only the Kitty Log page folds the revision into `settings_fp`, so a
            // sighting recorded while a DIFFERENT category is open changes no
            // pixels — scheduling a repaint there would be a wasted no-op frame.
            if on_page && let Some(w) = &ws.os_window {
                w.request_redraw();
            }
        }
    }

    /// Select the process-singleton Settings app, creating its first native tab
    /// when needed. About and Software Update are routes in this same app rather
    /// than modal/window variants, so their navigation, history, accessibility,
    /// and control inspection all share one semantic tree.
    pub(crate) fn open_settings_tab(
        &mut self,
        route: crate::native_settings::SettingsRoute,
    ) -> bool {
        let Some(wid) = self.frontmost_window else {
            return false;
        };
        // Singleton means one process controller, not one presentation. Search
        // only the requesting window; another window receives its own view over
        // the same instance and never has focus stolen from it.
        let existing = self
            .settings_tab_in_window(wid)
            .map(|(index, tab, instance, view)| (wid, index, tab, instance, view));

        let (wid, _index, tab_id, _instance, view) = if let Some(existing) = existing {
            existing
        } else {
            let snapshot = self.native_config_service.snapshot();
            let mut state = crate::native_settings::SettingsViewState::from_snapshot(&snapshot)
                .expect("versioned config service snapshots are valid Settings input");
            let _ = state.replace_title_summary_health(self.title_summary_health());
            state.navigate(route);
            let view_state = crate::native_app::AppViewState::Settings(Box::new(state));
            let presentation = crate::tab_model::TabPresentation {
                title: "Settings".to_string(),
                icon: Some(crate::tab_model::TabIconKind::Settings),
                indicators: crate::tab_model::TabIndicators::default(),
                closable: true,
                tooltip: Some(format!("Settings · {}", route.label())),
            };
            let install = if let Some(instance) = self
                .native_runtime
                .instance_by_kind(crate::native_app::AppKind::Settings)
            {
                self.install_native_tab(wid, instance, view_state, presentation)
                    .map(|(tab, view)| (instance, tab, view))
            } else {
                let checking = matches!(
                    self.native_updater_service.snapshot().phase,
                    crate::native_updater_service::UpdaterPhase::Checking
                        | crate::native_updater_service::UpdaterPhase::Available
                        | crate::native_updater_service::UpdaterPhase::Downloading
                );
                let app = crate::native_app::NativeApp::Settings(
                    crate::native_settings::SettingsApp::new_at_config_revision(
                        self.update_snapshot(checking),
                        snapshot.revision,
                    ),
                );
                self.install_new_native_tab(wid, app, view_state, presentation)
            };
            let Ok((instance, tab_id, view)) = install else {
                return false;
            };
            // The singleton controller is shared independently of any one
            // presentation. Attaching a view from `snapshot` is therefore the
            // synchronization point: update both its OCC base and the view
            // projection from the same canonical revision before either can
            // emit an edit. This also keeps staged/no-view lifecycle paths safe.
            let _ = self.native_runtime.dispatch(
                instance,
                view,
                crate::native_app::AppEvent::ConfigChanged(snapshot.clone()),
            );
            let index = self
                .windows
                .get(&wid)
                .and_then(|ws| ws.tab_set.active_index())
                .unwrap_or(0);
            (wid, index, tab_id, instance, view)
        };

        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.tab_set.switch_to(tab_id);
            ws.last_present = None;
        }
        self.sync_settings_title_summary_health();
        if route == crate::native_settings::SettingsRoute::SoftwareUpdate {
            self.acknowledge_native_update_attention();
        }
        // Seed + refresh the shared Packages projection whenever Settings
        // surfaces: publication is memory-only (the controller starts honest
        // "unobserved"), and the status collection runs on a worker thread —
        // never a status.toml parse on the event loop.
        self.publish_native_packages_state();
        self.start_native_packages_refresh();
        let action = crate::native_app::AppEvent::Action(crate::native_app::ActionInvocation {
            id: crate::native_ui::ActionId::new(format!("settings/route{}", route.path())),
            value: None,
        });
        // Reuse the exact human/semantic-action host path.  The old direct
        // runtime dispatch threw away `InvalidateOwnPresentation` and
        // `RepaintSelf`, leaving the model on the requested route while the
        // retained on-glass tray still showed the previous page.
        if self.dispatch_native_view_event(wid, view, action).is_err() {
            return false;
        }
        self.sync_active_session();
        if let Some(window) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
            window.focus_window();
            window.request_redraw();
        }
        true
    }

    /// Close every presentation of the native Settings singleton. Returns whether
    /// at least one tab was removed. The compatibility `settings false` control verb
    /// uses this exact path; no OS window or fake PTY is involved.
    pub(crate) fn close_settings_tabs(&mut self) -> bool {
        let targets: Vec<crate::WindowId> = self
            .windows
            .keys()
            .copied()
            .filter(|wid| self.settings_tab_in_window(*wid).is_some())
            .collect();
        let mut removed = false;
        for wid in targets {
            let Some((_, tab, _, _)) = self.settings_tab_in_window(wid) else {
                continue;
            };
            let previous = self.windows.get(&wid).and_then(|ws| ws.tab_set.active_id());
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.tab_set.switch_to(tab);
            }
            match self.close_active_native_tab(wid) {
                Ok(()) => removed = true,
                Err(_) => {
                    if let Some(previous) = previous
                        && let Some(ws) = self.windows.get_mut(&wid)
                    {
                        ws.tab_set.switch_to(previous);
                    }
                    self.sync_window(wid);
                }
            }
        }
        removed
    }

    /// Open the Settings overlay on the front window (no-op if already open). Snapshots
    /// the live config into the panel's control list.
    #[cfg(test)]
    pub(crate) fn settings_enter(&mut self) {
        // The modal steals the mouse: settle any in-flight divider/selection drag
        // first, or its swallowed release leaves the drag running under the panel.
        if let Some(wid) = self.frontmost_window {
            self.settle_pointer_drags(wid);
        }
        // Build the snapshot before the mutable window borrow (borrow split).
        let state = crate::settings::SettingsState::from_config_with_trail_pack_ids(
            &self.config,
            &self.config_assets.trail_packs.ids,
        );
        if let Some(ws) = self.settings_host_mut()
            && ws.settings().is_none()
        {
            // Mutual exclusion is STRUCTURAL now: assigning the one `overlay` slot drops
            // whatever About/Palette was there — no manual "clear the other two".
            ws.overlay = Some(crate::overlay::Overlay::Settings(state));
            if let Some(w) = &ws.os_window {
                w.request_redraw();
            }
        }
        // Open-time Kitty Log snapshot (§F4.6): memory only, no IO.
        self.sync_settings_kitty_log();
        self.overlay_a11y_update();
    }

    /// Close the Settings overlay on the front window (no-op if already closed). The
    /// `settings_fp` key term drops to `0`, so the next frame repaints the clean terminal.
    ///
    pub(crate) fn settings_exit(&mut self) {
        if let Some(ws) = self.settings_host_mut()
            && ws.settings().is_some()
        {
            ws.overlay = None;
            if let Some(w) = &ws.os_window {
                w.request_redraw();
            }
        }
        self.overlay_a11y_update();
    }

    /// The content band height in CELLS for a settings host window — the single band
    /// the painter, scroll clamps, and hit-test share ([`crate::settings::pane_geom_cells`]).
    /// Flat search rows are 1 cell tall, so the same number serves both modes.
    /// `pub(crate)`: the config-reload rebuild (`app_config.rs`) re-clamps with it too.
    pub(crate) fn settings_band(ws: &WindowState) -> usize {
        crate::settings::pane_geom_cells(ws.cols as usize, ws.settings_panel_rows()).group_band()
    }

    /// The footnote WRAP width (chars per row) for a settings host window — the
    /// same [`crate::settings::footnote_wrap_chars`] the painter derives from
    /// `cols`, threaded into every grouped-layout walk so scroll clamps and the
    /// keyboard walk agree with the painted rows (design §3.2 footnote wrap).
    pub(crate) fn settings_wrap(ws: &WindowState) -> usize {
        crate::settings::footnote_wrap_chars(ws.cols as usize)
    }

    /// ↑/↓: move the sidebar CATEGORY while the sidebar pane is focused, the flat
    /// filtered selection while searching, else the grouped content selection —
    /// keeping the target on-screen (design §6).
    pub(crate) fn settings_move(&mut self, delta: isize) {
        if let Some(ws) = self.settings_host_mut() {
            let band = Self::settings_band(ws);
            let wrap = Self::settings_wrap(ws);
            if let Some(s) = ws.settings_mut() {
                if s.filtering() {
                    s.move_selection(delta, band);
                } else if s.pane == crate::settings::SettingsPane::Sidebar {
                    s.sidebar_move(delta);
                } else {
                    s.move_selection_grouped(delta, band, wrap);
                }
            }
        }
        // A sidebar move may have landed on the Kitty Log page: refresh its
        // snapshot (§F4.6 — snapshot on category switch).
        self.sync_settings_kitty_log();
        self.settings_repaint_front();
    }

    /// Select control row `idx` directly (a mouse click / an a11y Focus action),
    /// clamped to the list. Outside search mode the CATEGORY follows the selection
    /// (an a11y client can focus any control) and the content pane takes focus.
    pub(crate) fn settings_select(&mut self, idx: usize) {
        if let Some(ws) = self.settings_host_mut() {
            let band = Self::settings_band(ws);
            let wrap = Self::settings_wrap(ws);
            if let Some(s) = ws.settings_mut() {
                s.selected = idx.min(s.fields.len().saturating_sub(1));
                s.status = None; // a selection move clears the transient status (§3.3)
                if s.filtering() {
                    s.clamp_scroll(band);
                } else {
                    // `selected` is in the category we set, so `set_category`'s snap
                    // keeps it — the category follows the selection, not vice versa.
                    if let Some(f) = s.fields.get(s.selected) {
                        s.set_category(crate::prefs::section_of(f.key));
                    }
                    s.pane = crate::settings::SettingsPane::Content;
                    s.clamp_group_scroll(band, wrap);
                }
            }
        }
        self.settings_repaint_front();
    }

    /// Activate sidebar category `sec` (a click on its row): sidebar takes focus, the
    /// content pane re-anchors to the category (scroll resets on change).
    pub(crate) fn settings_set_category(&mut self, sec: crate::prefs::Section) {
        if let Some(s) = self.settings_host_mut().and_then(|ws| ws.settings_mut()) {
            s.set_category(sec);
            s.pane = crate::settings::SettingsPane::Sidebar;
        }
        // Category-switch Kitty Log snapshot (§F4.6).
        self.sync_settings_kitty_log();
        self.settings_repaint_front();
    }

    /// →/Tab/↵ from the sidebar: give the content pane keyboard focus.
    pub(crate) fn settings_focus_content(&mut self) {
        if let Some(s) = self.settings_host_mut().and_then(|ws| ws.settings_mut()) {
            s.focus_content();
        }
        self.settings_repaint_front();
    }

    /// Esc/Tab from the content pane: give the sidebar keyboard focus.
    pub(crate) fn settings_focus_sidebar(&mut self) {
        if let Some(s) = self.settings_host_mut().and_then(|ws| ws.settings_mut()) {
            s.focus_sidebar();
        }
        self.settings_repaint_front();
    }

    /// Tab/⇧Tab: toggle keyboard focus between the two panes (design §6).
    pub(crate) fn settings_toggle_pane(&mut self) {
        let sidebar = self
            .settings_host()
            .and_then(|ws| ws.settings())
            .is_some_and(|s| s.pane == crate::settings::SettingsPane::Sidebar);
        if sidebar {
            self.settings_focus_content();
        } else {
            self.settings_focus_sidebar();
        }
    }

    /// Focus the settings SEARCH bar (`/` / Cmd-F): subsequent typing filters the list.
    pub(crate) fn settings_search_begin(&mut self) {
        if let Some(s) = self.settings_host_mut().and_then(|ws| ws.settings_mut()) {
            s.search_begin();
        }
        self.settings_repaint_front();
    }

    /// Append a character to the search query (keeping the selection visible + scrolled in).
    /// A fresh "kitty" completion in the query summons the §L.4 cameo over the
    /// sidebar — GUI only (a headless driver must not park a never-ticking cameo).
    pub(crate) fn settings_search_push(&mut self, c: char) {
        let headless = self.headless;
        if let Some(ws) = self.settings_host_mut() {
            let band = Self::settings_band(ws);
            if let Some(s) = ws.settings_mut() {
                s.search_push(c);
                s.clamp_scroll(band);
                if s.note_kitty_in_query() && !headless {
                    s.summon_kitty(crate::settings::KittyHost::Sidebar);
                }
            }
        }
        self.settings_repaint_front();
    }

    /// Delete the last search-query character.
    pub(crate) fn settings_search_backspace(&mut self) {
        if let Some(ws) = self.settings_host_mut() {
            let band = Self::settings_band(ws);
            if let Some(s) = ws.settings_mut() {
                s.search_backspace();
                s.clamp_scroll(band);
                let _ = s.note_kitty_in_query();
            }
        }
        self.settings_repaint_front();
    }

    /// Drop search-bar focus but KEEP the filter (Enter/↓ from the search bar → the list).
    /// An EMPTIED query confirms back into GROUPED mode (`search_confirm` re-anchors
    /// the category + re-zeroes the scroll unit); the grouped clamp then brings the
    /// selected row's box into view — mirrors `settings_search_clear`.
    pub(crate) fn settings_search_confirm(&mut self) {
        if let Some(ws) = self.settings_host_mut() {
            let band = Self::settings_band(ws);
            let wrap = Self::settings_wrap(ws);
            if let Some(s) = ws.settings_mut() {
                s.search_confirm();
                if !s.filtering() {
                    s.clamp_group_scroll(band, wrap);
                }
            }
        }
        self.settings_repaint_front();
    }

    /// Clear the filter and leave search (the single Esc level out of a filtered list).
    /// `search_clear` re-anchors the category on the selection; the grouped clamp then
    /// scrolls the selected row's box into view.
    pub(crate) fn settings_search_clear(&mut self) {
        if let Some(ws) = self.settings_host_mut() {
            let band = Self::settings_band(ws);
            let wrap = Self::settings_wrap(ws);
            if let Some(s) = ws.settings_mut() {
                s.search_clear();
                s.clamp_group_scroll(band, wrap);
            }
        }
        self.settings_repaint_front();
    }

    /// Leave the §L landing page for the two-pane panel (the Get-started bubble,
    /// ↵ on an empty box, Tab/↓, or a click on the bubble land here).
    pub(crate) fn settings_landing_get_started(&mut self) {
        if let Some(s) = self.settings_host_mut().and_then(|ws| ws.settings_mut()) {
            s.landing = false;
            s.status = None;
            s.pane = crate::settings::SettingsPane::Sidebar;
        }
        self.settings_repaint_front();
    }

    /// `settings section <name>` (socket): navigate the OPEN native Settings tab
    /// to the corresponding stable route. ERR when no Settings view is up.
    pub(crate) fn settings_show_section(
        &mut self,
        section: crate::prefs::Section,
    ) -> Result<(), String> {
        if !self.settings_tab_open() {
            return Err("settings not open (use: settings open)".to_string());
        }
        let route = match section {
            crate::prefs::Section::Appearance => crate::native_settings::SettingsRoute::Appearance,
            crate::prefs::Section::Cursor => crate::native_settings::SettingsRoute::CursorMotion,
            crate::prefs::Section::Typography => crate::native_settings::SettingsRoute::TextFonts,
            crate::prefs::Section::Window => crate::native_settings::SettingsRoute::WindowTabs,
            crate::prefs::Section::Input => crate::native_settings::SettingsRoute::KeyboardInput,
            crate::prefs::Section::Performance => {
                crate::native_settings::SettingsRoute::Performance
            }
            crate::prefs::Section::Terminal => crate::native_settings::SettingsRoute::Terminal,
            crate::prefs::Section::Security => crate::native_settings::SettingsRoute::Security,
            crate::prefs::Section::Packages => crate::native_settings::SettingsRoute::Packages,
            crate::prefs::Section::KittyLog => crate::native_settings::SettingsRoute::Diagnostics,
        };
        self.open_settings_tab(route)
            .then_some(())
            .ok_or_else(|| "could not focus the native Settings tab".to_string())
    }

    /// Landing ↵: a non-empty suggestion box SENDS; an empty one is Get started
    /// (the hero's one Enter affordance stays useful either way).
    pub(crate) fn settings_landing_confirm(&mut self) {
        let has_text = self
            .settings_host()
            .and_then(|ws| ws.settings())
            .is_some_and(|s| !s.comment.trim().is_empty());
        if has_text {
            self.settings_comment_send();
        } else {
            self.settings_landing_get_started();
        }
    }

    /// Append to the landing suggestion box (§L.3). A fresh "kitty" completion
    /// summons the cameo (§L.4) — GUI only: a headless driver typing into the
    /// box must not park a never-ticking cameo in the fingerprint.
    pub(crate) fn settings_comment_push(&mut self, c: char) {
        let headless = self.headless;
        if let Some(s) = self.settings_host_mut().and_then(|ws| ws.settings_mut()) {
            // Generous-but-bounded buffer: the prefill URL stays a sane length.
            if s.comment.chars().count() < 400 {
                s.comment.push(c);
            }
            s.status = None;
            if s.note_kitty_in_comment() && !headless {
                s.summon_kitty(crate::settings::KittyHost::Landing);
            }
        }
        self.settings_repaint_front();
    }

    /// Delete the last suggestion-box character (the kitty high-water count
    /// follows DOWN so deleting + retyping summons again).
    pub(crate) fn settings_comment_backspace(&mut self) {
        if let Some(s) = self.settings_host_mut().and_then(|ws| ws.settings_mut()) {
            s.comment.pop();
            s.status = None;
            let _ = s.note_kitty_in_comment();
        }
        self.settings_repaint_front();
    }

    /// Send the §L.3 suggestion: open the PREFILLED anonymous suggestion form in
    /// the default browser (`open_url_external` — the same helper link clicks
    /// use). The overlay itself never talks to the network; submitting is the
    /// user's explicit second step in the browser, and the form collects no
    /// identity. The buffer clears optimistically with a footer confirmation.
    pub(crate) fn settings_comment_send(&mut self) {
        let text = self
            .settings_host()
            .and_then(|ws| ws.settings())
            .map(|s| s.comment.trim().to_string())
            .unwrap_or_default();
        if text.is_empty() {
            return;
        }
        let url = format!(
            "{SUGGEST_FORM_URL}?usp=pp_url&{SUGGEST_FORM_FIELD}={}",
            percent_encode(&text)
        );
        crate::app_mouse::open_url_external(&url);
        if let Some(s) = self.settings_host_mut().and_then(|ws| ws.settings_mut()) {
            s.comment.clear();
            s.comment_kitties = 0;
            s.status = Some(
                "Opening your browser — press Submit there to send it anonymously.".to_string(),
            );
        }
        self.settings_repaint_front();
    }

    /// Reset the SELECTED control to its built-in default (Del / Cmd-Backspace): persist
    /// the key as REMOVED (`None`) through the same atomic writer + `Wake::ConfigReload`
    /// live-apply the edits use, then optimistically clear the row's seed so it shows the
    /// default this frame.
    pub(crate) fn settings_reset_selected(&mut self) {
        let key = self
            .settings_host()
            .and_then(|ws| ws.settings())
            .and_then(|s| s.action_target().and_then(|i| s.fields.get(i)))
            .map(|f| f.key);
        let Some(key) = key else { return };

        let outcome = crate::prefs::save_prefs_edits(&[(key, None)]);
        let persisted = matches!(outcome, crate::prefs::SaveOutcome::Saved);
        let status = match &outcome {
            crate::prefs::SaveOutcome::Saved => {
                if let Some(proxy) = self.proxy.as_ref() {
                    let _ = proxy.send_event(Wake::ConfigReload);
                }
                format!("reset: {key} = (default)")
            }
            crate::prefs::SaveOutcome::Unchanged => format!("{key}: already default"),
            crate::prefs::SaveOutcome::Error(e) => format!("reset failed: {e}"),
        };
        if let Some(ws) = self.settings_host_mut() {
            if let Some(s) = ws.settings_mut() {
                if persisted {
                    // Clear the row's seed optimistically: display_value falls back
                    // to the placeholder (the effective default), so the row shows
                    // the just-applied reset this frame instead of the old value
                    // until the ConfigReload rebuild lands.
                    for f in s.fields.iter_mut().filter(|f| f.key == key) {
                        f.seed = None;
                    }
                }
                s.status = Some(status);
            }
            if let Some(w) = &ws.os_window {
                w.request_redraw();
            }
        }
        self.overlay_a11y_update();
    }

    /// ACTIVATE the selected control: a popup-chip row (Theme / long Enum) opens its
    /// anchored MENU; a Color row opens the COLOUR WHEEL popover (design §7); a
    /// Bool/short-Enum toggles/cycles and persists via the shared
    /// [`Self::settings_commit_value`] seam; a free-form row (Float/Integer/Text)
    /// opens the in-panel text editor.
    pub(crate) fn settings_activate(&mut self) {
        // A wheel scroll moves the band without the selection; a keyboard gesture
        // acts ON the selection, so first snap the band back onto it — otherwise
        // Enter would mutate an off-screen row and the popup menu would anchor to a
        // row outside the band. Same view-follows-selection rule as ↑/↓.
        self.settings_rescue_selection();
        // Popup rows open the menu with the current value highlighted, NEVER cycle —
        // cycling from a custom (non-registry) theme value would silently destroy it.
        let popup = self
            .settings_host()
            .and_then(|ws| ws.settings())
            .and_then(|s| s.action_target().and_then(|i| s.fields.get(i)))
            .is_some_and(crate::settings::uses_popup);
        if popup {
            self.settings_menu_open();
            return;
        }
        // Colour rows open the wheel popover — the free-text editor route is
        // retired for Color only (the inline well still shows swatch + hex).
        let color = self
            .settings_host()
            .and_then(|ws| ws.settings())
            .and_then(|s| s.action_target().and_then(|i| s.fields.get(i)))
            .is_some_and(|f| matches!(f.kind, crate::prefs::EditKind::Color));
        if color {
            self.settings_wheel_open();
            return;
        }
        let edit = self
            .settings_host()
            .and_then(|ws| ws.settings())
            .and_then(|s| s.action_target().and_then(|i| s.fields.get(i)))
            .and_then(crate::settings::cycle_edit);
        let Some((key, val)) = edit else {
            // Not a cycle/toggle row → it's a free-form Text/Float/Integer control;
            // Enter/Space opens the in-panel text editor instead of no-op'ing.
            self.settings_edit_begin();
            return;
        };
        self.settings_commit_value(key, val);
    }

    /// Persist ONE control value through the shared seam — the single commit path the
    /// activate (toggle/cycle), popup-menu, and ←/→ step gestures all funnel into:
    /// [`crate::prefs::save_prefs_edits`] (pure, atomic, format-preserving), then
    /// `Wake::ConfigReload` to apply live (the re-theme IS the preview), a footer status,
    /// and an optimistic seed update so the row reflects the value THIS frame (the
    /// authoritative rebuild follows when the reload lands; both produce the same seed).
    pub(crate) fn settings_commit_value(
        &mut self,
        key: &'static str,
        val: Option<String>,
    ) -> String {
        // Already the stored raw value → skip the writer outright. This is what makes
        // committing a preserved CUSTOM entry a true no-op: an unrecognized enum
        // spelling would otherwise be domain-REJECTED by the writer even though it is
        // the value already on disk.
        let unchanged = self
            .settings_host()
            .and_then(|ws| ws.settings())
            .and_then(|s| s.fields.iter().find(|f| f.key == key))
            .is_some_and(|f| f.seed == val);
        if unchanged {
            if let Some(ws) = self.settings_host_mut() {
                if let Some(s) = ws.settings_mut() {
                    s.status = Some(format!("{key}: unchanged"));
                }
                if let Some(w) = &ws.os_window {
                    w.request_redraw();
                }
            }
            self.overlay_a11y_update();
            return format!("{key}: unchanged");
        }

        // A HUD panel switched ON while the master `show_hud` is off would edit the
        // file yet visibly do nothing (the reload applies `master && per-panel`), so
        // the gesture revives the master in the SAME atomic write — the overlay row
        // thereby shares the View-menu / Performance-panel `user_set_panel` semantics.
        let mut edits: Vec<(&str, Option<String>)> = vec![(key, val.clone())];
        let revive_master = val.as_deref() == Some("true")
            && crate::hud_bar::PanelId::ALL
                .iter()
                .any(|p| p.config_key() == key)
            && !self.config.show_hud_or_default();
        if revive_master {
            self.config.show_hud = Some(true);
            edits.push((crate::prefs::EDIT_SHOW_HUD, Some("true".to_string())));
        }

        // Persist through the shared pure, atomic, format-preserving writer. A clone
        // keeps `val` for the optimistic snapshot below.
        let outcome = crate::prefs::save_prefs_edits(&edits);
        let persisted = matches!(outcome, crate::prefs::SaveOutcome::Saved);
        let status = match &outcome {
            crate::prefs::SaveOutcome::Saved => {
                // Apply live exactly like the config-watcher — the identical wake.
                if let Some(proxy) = self.proxy.as_ref() {
                    let _ = proxy.send_event(Wake::ConfigReload);
                }
                format!("saved: {key} = {}", val.as_deref().unwrap_or(""))
            }
            crate::prefs::SaveOutcome::Unchanged => format!("{key}: unchanged"),
            crate::prefs::SaveOutcome::Error(e) => format!("save failed: {e}"),
        };

        // Optimistic update keyed by `key` (not `selected`) so every commit gesture —
        // including a menu whose anchor could drift from the selection — hits its row.
        if let Some(ws) = self.settings_host_mut() {
            if let Some(s) = ws.settings_mut() {
                if persisted {
                    // Re-seed by KEY (not `selected`) so every commit gesture —
                    // including a popover whose anchor could drift from the
                    // selection — hits its row this frame, ahead of the
                    // ConfigReload rebuild.
                    for f in s.fields.iter_mut().filter(|f| f.key == key) {
                        f.seed = val.clone();
                    }
                }
                if revive_master
                    && persisted
                    && let Some(f) = s
                        .fields
                        .iter_mut()
                        .find(|f| f.key == crate::prefs::EDIT_SHOW_HUD)
                {
                    f.seed = Some("true".to_string());
                }
                s.status = Some(status.clone());
            }
            if let Some(w) = &ws.os_window {
                w.request_redraw();
            }
        }
        self.overlay_a11y_update();
        status
    }

    /// `settings set|unset <key> …` (control socket): commit ONE settings field BY
    /// KEY through the exact validated seam every overlay gesture funnels into —
    /// [`Self::settings_commit_value`] → `save_prefs_edits` (pure, atomic, domain-
    /// checked, format-preserving) → `Wake::ConfigReload` live-apply. Works with
    /// the overlay closed and headless; keys are what `controls prefs` prints. A
    /// domain-rejected write comes back as the writer's own `save failed: …`.
    pub(crate) fn set_settings_field(
        &mut self,
        key: &str,
        val: Option<String>,
    ) -> Result<String, String> {
        let Some(field) = crate::prefs::editable_fields(&self.config)
            .into_iter()
            .find(|f| f.key == key)
        else {
            return Err(format!(
                "unknown key {key:?} (list keys with `controls prefs`)"
            ));
        };
        let status = self.settings_commit_value(field.key, val);
        if status.starts_with("save failed") {
            Err(status)
        } else {
            Ok(status)
        }
    }

    /// The live [`crate::settings::SettingsGeom`] of the front window's settings card —
    /// the SAME cell/font/row numbers `splice_settings_panel` paints with, consumed by
    /// the menu placement + mouse hit-test paths. `None` when the overlay is closed.
    pub(crate) fn settings_geom_front(&self) -> Option<crate::settings::SettingsGeom> {
        let ws = self.settings_host()?;
        ws.settings()?;
        let (cw, ch) = self.cell_size();
        Some(crate::settings::SettingsGeom {
            cw: cw as f32,
            ch: ch as f32,
            font_px: self.font_px,
            cols: ws.cols as usize,
            panel_rows: ws.settings_panel_rows(),
        })
    }

    /// The open menu's on-screen option-row count (its scroll window), from the SAME
    /// [`crate::settings::menu_geom`] the painter and hit-test use. `1` when unknown.
    fn settings_menu_visible(&self) -> usize {
        let Some(geom) = self.settings_geom_front() else {
            return 1;
        };
        self.settings_host()
            .and_then(|ws| ws.settings())
            .and_then(|s| crate::settings::menu_geom(s, &geom))
            .map_or(1, |mg| mg.visible)
    }

    /// Open the popup menu on the selected row (Enter/Space/click on a popup chip).
    pub(crate) fn settings_menu_open(&mut self) {
        let opened = self
            .settings_host_mut()
            .and_then(|ws| ws.settings_mut())
            .is_some_and(crate::settings::SettingsState::menu_open);
        if opened {
            // Snap the menu's scroll window onto the highlighted (current) entry.
            let visible = self.settings_menu_visible();
            if let Some(s) = self.settings_host_mut().and_then(|ws| ws.settings_mut()) {
                s.menu_move(0, visible);
            }
        }
        self.settings_repaint_front();
    }

    /// Close the popup menu with NO change (Esc / click-away).
    pub(crate) fn settings_menu_cancel(&mut self) {
        if let Some(s) = self.settings_host_mut().and_then(|ws| ws.settings_mut()) {
            s.menu_cancel();
        }
        self.settings_repaint_front();
    }

    /// Move the popup menu highlight by `delta` (clamped, no wrap).
    pub(crate) fn settings_menu_move(&mut self, delta: isize) {
        let visible = self.settings_menu_visible();
        if let Some(s) = self.settings_host_mut().and_then(|ws| ws.settings_mut()) {
            s.menu_move(delta, visible);
        }
        self.settings_repaint_front();
    }

    /// Jump the popup menu highlight to the next option starting with `c`.
    pub(crate) fn settings_menu_jump(&mut self, c: char) {
        let visible = self.settings_menu_visible();
        if let Some(s) = self.settings_host_mut().and_then(|ws| ws.settings_mut()) {
            s.menu_jump(c, visible);
        }
        self.settings_repaint_front();
    }

    /// Wheel-scroll the popup menu's option window by `delta` rows.
    pub(crate) fn settings_menu_scroll(&mut self, delta: isize) {
        let visible = self.settings_menu_visible();
        if let Some(s) = self.settings_host_mut().and_then(|ws| ws.settings_mut()) {
            s.menu_scroll_by(delta, visible);
        }
        self.settings_repaint_front();
    }

    /// Commit the popup menu's highlighted option: close the menu, then persist through
    /// the SAME seam as activate ([`Self::settings_commit_value`]). Committing the
    /// already-current entry (including a preserved custom value) closes with no change.
    pub(crate) fn settings_menu_commit(&mut self) {
        let pending = self
            .settings_host()
            .and_then(|ws| ws.settings())
            .and_then(crate::settings::SettingsState::menu_pending);
        if let Some(s) = self.settings_host_mut().and_then(|ws| ws.settings_mut()) {
            s.menu_cancel();
        }
        match pending {
            Some((key, val)) => {
                self.settings_commit_value(key, val);
            }
            None => self.settings_repaint_front(),
        }
    }

    /// Open the colour-wheel popover on the selected Color row (↵/Space or a
    /// widget-region click — the route that replaced the free-text editor for
    /// Color rows, design §7). The model seeds from the row's effective hex; an
    /// unset key falls back to the live theme's colour FOR THAT KEY
    /// ([`crate::settings::theme_color_for_key`]: fg/bg/cursor/selection) — the
    /// App reads the theme, the pure model never does. Seeding the accent for
    /// every key made opening the wheel on an unset Background instantly preview
    /// (and one ↵ persist) the cursor green.
    pub(crate) fn settings_wheel_open(&mut self) {
        let key = self
            .settings_host()
            .and_then(|ws| ws.settings())
            .and_then(|s| s.action_target().and_then(|i| s.fields.get(i)))
            .map_or("", |f| f.key);
        let fallback = crate::settings::theme_color_for_key(self.theme, key);
        if let Some(s) = self.settings_host_mut().and_then(|ws| ws.settings_mut()) {
            s.wheel_open(fallback);
        }
        self.settings_repaint_front();
    }

    /// Close the colour wheel with NO change (Esc / click-away) — the working
    /// colour is discarded; nothing was written while scrubbing.
    pub(crate) fn settings_wheel_cancel(&mut self) {
        if let Some(s) = self.settings_host_mut().and_then(|ws| ws.settings_mut()) {
            s.wheel_cancel();
        }
        self.settings_repaint_front();
    }

    /// Commit the wheel's working colour (↵): close the popover, then persist the
    /// canonical `#RRGGBB` (or `None` for an emptied hex — reset to the theme
    /// default) ONCE through the UNCHANGED [`Self::settings_commit_value`] seam —
    /// the same path every widget uses.
    pub(crate) fn settings_wheel_commit(&mut self) {
        let pending = self
            .settings_host()
            .and_then(|ws| ws.settings())
            .and_then(crate::settings::SettingsState::wheel_pending);
        if let Some(s) = self.settings_host_mut().and_then(|ws| ws.settings_mut()) {
            s.wheel_cancel();
        }
        match pending {
            Some((key, val)) => {
                self.settings_commit_value(key, val);
            }
            None => self.settings_repaint_front(),
        }
    }

    /// Tab inside the wheel popover: cycle keyboard focus Wheel → Value → Hex.
    pub(crate) fn settings_wheel_focus_next(&mut self) {
        if let Some(s) = self.settings_host_mut().and_then(|ws| ws.settings_mut()) {
            s.wheel_focus_next();
        }
        self.settings_repaint_front();
    }

    /// Arrow-key adjust of the wheel's focused sub-control (`big` = Shift): hue/
    /// saturation on the disk, brightness on the value slider (design §7).
    pub(crate) fn settings_wheel_arrow(&mut self, dx: f32, dy: f32, big: bool) {
        if let Some(s) = self.settings_host_mut().and_then(|ws| ws.settings_mut()) {
            s.wheel_arrow(dx, dy, big);
        }
        self.settings_repaint_front();
    }

    /// Type into the wheel's hex readout (no-op unless the hex field has focus).
    pub(crate) fn settings_wheel_hex_push(&mut self, c: char) {
        if let Some(s) = self.settings_host_mut().and_then(|ws| ws.settings_mut()) {
            s.wheel_hex_push(c);
        }
        self.settings_repaint_front();
    }

    /// Delete the last hex character (no-op unless the hex field has focus).
    pub(crate) fn settings_wheel_hex_backspace(&mut self) {
        if let Some(s) = self.settings_host_mut().and_then(|ws| ws.settings_mut()) {
            s.wheel_hex_backspace();
        }
        self.settings_repaint_front();
    }

    /// A press ON the wheel's disk: set (h, s) from the polar point, give the disk
    /// keyboard focus, and ARM the drag — motion keeps scrubbing until release.
    pub(crate) fn settings_wheel_press_disk(&mut self, h: f32, s: f32) {
        if let Some(st) = self.settings_host_mut().and_then(|ws| ws.settings_mut()) {
            st.wheel_set_hs(h, s);
            if let Some(w) = st.wheel.as_mut() {
                w.focus = crate::settings::WheelFocus::Wheel;
                w.drag = Some(crate::settings::WheelDrag::Disk);
            }
        }
        self.settings_repaint_front();
    }

    /// A press ON the wheel's value slider: set `v` from the track x, focus it, and
    /// arm the slider drag.
    pub(crate) fn settings_wheel_press_slider(&mut self, v: f32) {
        if let Some(st) = self.settings_host_mut().and_then(|ws| ws.settings_mut()) {
            st.wheel_set_v(v);
            if let Some(w) = st.wheel.as_mut() {
                w.focus = crate::settings::WheelFocus::Value;
                w.drag = Some(crate::settings::WheelDrag::Slider);
            }
        }
        self.settings_repaint_front();
    }

    /// A click on the wheel's hex readout: give it keyboard focus (typing edits it).
    pub(crate) fn settings_wheel_focus_hex(&mut self) {
        if let Some(w) = self
            .settings_host_mut()
            .and_then(|ws| ws.settings_mut())
            .and_then(|s| s.wheel.as_mut())
        {
            w.focus = crate::settings::WheelFocus::Hex;
        }
        self.settings_repaint_front();
    }

    /// End an in-flight wheel scrub (left release): the working colour keeps its
    /// last dragged value; nothing persists until ↵. No-op when nothing is held.
    pub(crate) fn settings_wheel_drag_end(&mut self) {
        if let Some(w) = self
            .settings_host_mut()
            .and_then(|ws| ws.settings_mut())
            .and_then(|s| s.wheel.as_mut())
        {
            w.drag = None;
        }
    }

    /// ←/→ IN-PLACE adjust of the selected control (design §6): toggle a Bool, step an
    /// Enum/Theme to its prev/next option (custom value included, so it is stepped FROM
    /// rather than clobbered), nudge a bounded numeric one step (`big` = Shift = ×10)
    /// clamped to its range — each press committing via the shared seam. Free-form rows
    /// no-op ([`crate::settings::step_edit`]).
    pub(crate) fn settings_step(&mut self, delta: isize, big: bool) {
        // Same rescue as `settings_activate`: ←/→ act on the selection, which a wheel
        // scroll may have moved out of the band.
        self.settings_rescue_selection();
        let edit = self
            .settings_host()
            .and_then(|ws| ws.settings())
            .and_then(|s| {
                let f = s.action_target().and_then(|i| s.fields.get(i))?;
                // Thread the loaded Trail Pack ids so ←/→ cycles the pack options.
                crate::settings::step_edit_with(f, delta, big, &s.trail_pack_ids)
            });
        match edit {
            Some((key, val)) => {
                self.settings_commit_value(key, val);
            }
            // No-op row: still repaint — the rescue may have moved the scroll window.
            None => self.settings_repaint_front(),
        }
    }

    /// Wheel-scroll the content band by `delta` rows — moves the scroll window WITHOUT
    /// touching the selection (the wash may leave the band). Grouped or flat per mode.
    pub(crate) fn settings_scroll_body(&mut self, delta: isize) {
        if let Some(ws) = self.settings_host_mut() {
            let band = Self::settings_band(ws);
            let wrap = Self::settings_wrap(ws);
            if let Some(s) = ws.settings_mut() {
                if s.filtering() {
                    s.scroll_body(delta, band);
                } else {
                    s.scroll_grouped(delta, band, wrap);
                }
            }
        }
        self.settings_repaint_front();
    }

    /// Snap the scroll window back onto the SELECTED control before a keyboard
    /// gesture acts on it (the clamp brings an off-band selection back into view in
    /// either direction). Wheel scrolling deliberately leaves the selection behind;
    /// every mutating gesture routes through here first so it never operates on a row
    /// the user cannot see.
    fn settings_rescue_selection(&mut self) {
        if let Some(ws) = self.settings_host_mut() {
            let band = Self::settings_band(ws);
            let wrap = Self::settings_wrap(ws);
            if let Some(s) = ws.settings_mut() {
                if s.filtering() {
                    s.clamp_scroll(band);
                } else {
                    s.clamp_group_scroll(band, wrap);
                }
            }
        }
    }

    /// Begin editing the selected free-form control (Text/Float/Integer) — opens the
    /// in-panel text editor seeded with the configured value. No-op on Bool/Enum rows
    /// (those cycle via [`Self::settings_activate`]) or when already editing.
    pub(crate) fn settings_edit_begin(&mut self) {
        let began = self
            .settings_host_mut()
            .and_then(|ws| ws.settings_mut())
            .is_some_and(crate::settings::SettingsState::edit_begin);
        if began {
            self.settings_repaint_front();
        }
    }

    /// Append a typed character to the in-panel edit buffer.
    pub(crate) fn settings_edit_push(&mut self, c: char) {
        if let Some(s) = self.settings_host_mut().and_then(|ws| ws.settings_mut()) {
            s.edit_push(c);
        }
        self.settings_repaint_front();
    }

    /// Delete the last character of the in-panel edit buffer.
    pub(crate) fn settings_edit_backspace(&mut self) {
        if let Some(s) = self.settings_host_mut().and_then(|ws| ws.settings_mut()) {
            s.edit_backspace();
        }
        self.settings_repaint_front();
    }

    /// Abandon the in-progress edit (Esc), reverting to the displayed value.
    pub(crate) fn settings_edit_cancel(&mut self) {
        if let Some(s) = self.settings_host_mut().and_then(|ws| ws.settings_mut()) {
            s.edit_cancel();
        }
        self.settings_repaint_front();
    }

    /// Commit the in-panel edit (Enter): persist the typed value through the SAME prefs
    /// seam + `Wake::ConfigReload`. A rejected value (bad number) sets a status message
    /// and STAYS in edit mode so the user can fix it; a clean commit leaves edit mode.
    pub(crate) fn settings_edit_commit(&mut self) {
        let pending = self
            .settings_host()
            .and_then(|ws| ws.settings())
            .and_then(crate::settings::SettingsState::edit_pending);
        let Some((key, val)) = pending else { return };

        let outcome = crate::prefs::save_prefs_edits(&[(key, val.clone())]);
        // Saved → update the optimistic seed + leave edit; Unchanged → leave edit (the
        // value already matched); Error → stay in edit mode so the bad value can be fixed.
        let (update_seed, leave_edit) = match &outcome {
            crate::prefs::SaveOutcome::Saved => (true, true),
            crate::prefs::SaveOutcome::Unchanged => (false, true),
            crate::prefs::SaveOutcome::Error(_) => (false, false),
        };
        let status = match &outcome {
            crate::prefs::SaveOutcome::Saved => {
                if let Some(proxy) = self.proxy.as_ref() {
                    let _ = proxy.send_event(Wake::ConfigReload);
                }
                match val.as_deref() {
                    Some(v) => format!("saved: {key} = {v}"),
                    None => format!("saved: {key} = (default)"),
                }
            }
            crate::prefs::SaveOutcome::Unchanged => format!("{key}: unchanged"),
            crate::prefs::SaveOutcome::Error(e) => format!("invalid {key}: {e}"),
        };

        if let Some(ws) = self.settings_host_mut() {
            if let Some(s) = ws.settings_mut() {
                if update_seed && let Some(f) = s.fields.get_mut(s.selected) {
                    f.seed = val;
                }
                if leave_edit {
                    s.editing = None;
                }
                s.status = Some(status);
            }
            if let Some(w) = &ws.os_window {
                w.request_redraw();
            }
        }
        self.overlay_a11y_update();
    }

    /// Request a redraw of the front window so the panel's state change is presented.
    /// The change is carried by `RepaintKey::settings_fp` (see [`crate::settings::SettingsState::fingerprint`]),
    /// so this only needs to ask winit for a frame — no early-out side-channel. Also
    /// pushes the updated accessibility tree (a no-op without the `a11y-accesskit`
    /// feature / no attached screen reader).
    fn settings_repaint_front(&mut self) {
        if let Some(ws) = self.settings_host_mut()
            && let Some(w) = &ws.os_window
        {
            w.request_redraw();
        }
        self.overlay_a11y_update();
    }

    /// Push the FRONT window's OPEN overlay accessibility tree (Settings / About / Palette /
    /// Update, or an empty root when closed) to its AccessKit adapter. A no-op without the
    /// `a11y-accesskit` feature; under it, a no-op unless a screen reader is attached
    /// (`update_if_active`). Called by every overlay mutator, so opening/mutating ANY surface
    /// refreshes the tree — not just Settings.
    pub(crate) fn overlay_a11y_update(&mut self) {
        #[cfg(feature = "a11y-accesskit")]
        if let Some(wid) = self.frontmost_window {
            self.push_a11y_tree(wid);
        }
    }

    /// Build the accessibility tree for window `wid`'s OPEN overlay (Settings / About /
    /// Palette / Update) — or an empty root when nothing is open — and hand it to that
    /// window's AccessKit adapter. The tree fans out through [`crate::overlay::OverlayModel::a11y`]
    /// off the SAME model the pixels + `controls` verb read, so which surface is live routes
    /// itself (a missing variant is a compile error, not a Settings-only fallback to empty).
    #[cfg(feature = "a11y-accesskit")]
    pub(crate) fn push_a11y_tree(&mut self, wid: crate::WindowId) {
        if self.windows.get(&wid).is_none_or(|ws| ws.a11y.is_none()) {
            return;
        }
        // DEFAULT-2: with NO overlay open, publish the live terminal GRID (its visible text,
        // read-only) so a screen reader reads the terminal itself — previously this handed
        // AccessKit an empty root and VoiceOver announced nothing. Build the grid snapshot
        // BEFORE the mutable window borrow below (both borrow `self`). An overlay keeps its
        // own tree.
        let has_overlay = self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.overlay.is_some());
        // A native frame stages its projection from the exact CompiledUi used for paint.
        // Initial-tree requests before first paint compile one projection here. `None`
        // means the front content is terminal; `Some(Err)` is a native projection failure
        // and must fail closed rather than announce a hidden terminal grid.
        let native_update = (!has_overlay)
            .then(|| self.take_native_accessibility_update(wid))
            .flatten();
        let grid_snap = (!has_overlay && native_update.is_none())
            .then(|| self.grid_snapshot(wid))
            .flatten();
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        // Build eagerly to an owned value BEFORE the mutable adapter borrow: the immutable
        // `&ws.overlay` field borrow ends once `update` is materialised, freeing `ws.a11y`.
        let (update, published) = match &ws.overlay {
            Some(o) => (crate::overlay::OverlayModel::a11y(o.model()), None),
            None => match native_update {
                Some(Ok((update, published))) => (update, Some(published)),
                Some(Err(_)) => (crate::accesskit_tree::empty_tree(), None),
                None => match &grid_snap {
                    Some(snap) => (crate::accesskit_tree::grid_tree(snap), None),
                    None => (crate::accesskit_tree::empty_tree(), None),
                },
            },
        };
        ws.native_a11y_published = published;
        if ws.native_a11y_published.is_none() {
            ws.native_a11y_staged = None;
        }
        let Some(adapter) = ws.a11y.as_mut() else {
            return;
        };
        adapter.update_if_active(move || update);
    }

    /// P2: handle an event from a window's AccessKit adapter (delivered as
    /// `Wake::Accessibility`): the OS a11y client requesting the initial tree, an action
    /// request from a screen reader, or deactivation.
    #[cfg(feature = "a11y-accesskit")]
    pub(crate) fn on_accessibility_event(&mut self, event: accesskit_winit::Event) {
        use accesskit_winit::WindowEvent as AkWindowEvent;
        let Some(wid) = self.winit_to_window.get(&event.window_id).copied() else {
            return;
        };
        match event.window_event {
            // The OS a11y client is initialising — hand it the live overlay's tree.
            AkWindowEvent::InitialTreeRequested => self.push_a11y_tree(wid),
            // A screen reader activated/focused a control → drive the live overlay's model.
            AkWindowEvent::ActionRequested(req) => self.on_accessibility_action(wid, req),
            AkWindowEvent::AccessibilityDeactivated => {}
        }
    }

    /// Route an OS accessibility `ActionRequest` to whichever overlay is live on `wid`,
    /// branching on [`crate::overlay::OverlayKind`] so each surface decodes the request with
    /// the SAME id scheme its `a11y()` builder minted (a mismatch would silently misroute a
    /// screen-reader Click):
    /// - **Settings** — node id `field_index + 1`: Focus selects the row, Click activates it
    ///   (toggle / cycle / begin-edit), exactly like a keyboard/mouse activate.
    /// - **About** — the lone OK button carries Click → close the dialog.
    /// - **Update** — the button id maps back to its [`crate::update_screen::UpdateHit`]
    ///   (Close / Check / Install) via [`crate::update_screen::a11y_hit`].
    /// - **Palette** — a filtered row id contains its current target-set epoch and slot:
    ///   Focus moves the cursor, Click selects then activates the command (a disabled row
    ///   carries no Click, and a delayed request from an old tab/generation is rejected).
    #[cfg(feature = "a11y-accesskit")]
    fn on_accessibility_action(&mut self, wid: crate::WindowId, req: accesskit::ActionRequest) {
        use crate::overlay::OverlayKind;
        let kind = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.overlay.as_ref())
            .map(crate::overlay::Overlay::kind);
        let Some(kind) = kind else {
            self.on_native_accessibility_action(wid, req);
            return;
        };
        match kind {
            #[cfg(test)]
            OverlayKind::Settings => {
                let Some(idx) = (req.target_node.0 as usize).checked_sub(1) else {
                    return; // the window root carries no control
                };
                match req.action {
                    accesskit::Action::Focus => self.settings_select(idx),
                    accesskit::Action::Click => {
                        self.settings_select(idx);
                        self.settings_activate();
                    }
                    _ => {}
                }
            }
            #[cfg(test)]
            OverlayKind::About => {
                // Two actionable nodes: the site Link opens the browser; any other
                // Click (the OK button) closes — matching the pointer's hit map.
                if req.action == accesskit::Action::Click {
                    let site = self
                        .windows
                        .get(&wid)
                        .and_then(|ws| ws.about())
                        .and_then(crate::about::site_node_id);
                    if site == Some(req.target_node.0) {
                        self.open_about_site(wid);
                    } else {
                        self.about_exit(wid);
                    }
                }
            }
            #[cfg(test)]
            OverlayKind::Update => {
                if req.action == accesskit::Action::Click
                    && let Some(hit) = crate::update_screen::a11y_hit(req.target_node)
                {
                    self.update_screen_click(wid, hit);
                }
            }
            OverlayKind::Palette => {
                let Some(idx) = self
                    .windows
                    .get(&wid)
                    .and_then(|ws| ws.palette())
                    .and_then(|palette| palette.a11y_filtered_index(req.target_node))
                else {
                    return; // root / list container carries no row action
                };
                match req.action {
                    accesskit::Action::Focus => {
                        if let Some(p) = self.windows.get_mut(&wid).and_then(|ws| ws.palette_mut())
                        {
                            p.select(idx);
                        }
                        self.overlay_a11y_update();
                        if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref())
                        {
                            w.request_redraw();
                        }
                    }
                    accesskit::Action::Click => {
                        if let Some(p) = self.windows.get_mut(&wid).and_then(|ws| ws.palette_mut())
                        {
                            p.select(idx);
                        }
                        self.palette_activate();
                    }
                    _ => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use crate::App;
    use crate::native_app::{ActionInvocation, AppEffect, AppEvent, SemanticInput};
    use crate::native_ui::ActionId;
    use crate::settings::{canonical_hex, u32_rgb};

    #[derive(Clone, Copy, Debug)]
    enum CursorDependent {
        Rainbow,
        Droplet,
        BeamRod,
        Fireball,
        Comet,
        Phaser,
        Trail,
        Cat,
        WordDecoration,
    }

    const CURSOR_DEPENDENTS: [CursorDependent; 9] = [
        CursorDependent::Rainbow,
        CursorDependent::Droplet,
        CursorDependent::BeamRod,
        CursorDependent::Fireball,
        CursorDependent::Comet,
        CursorDependent::Phaser,
        CursorDependent::Trail,
        CursorDependent::Cat,
        CursorDependent::WordDecoration,
    ];

    fn activate_non_word_dependent(
        ws: &mut crate::WindowState,
        effect: CursorDependent,
        now: Instant,
    ) {
        let geom = crate::cursor_glow::Geom {
            cw: 8,
            ch: 16,
            rows: 24,
            cols: 80,
            origin_x: 0,
            origin_y: 0,
            win_w: 640,
            win_h: 384,
            head: 0,
        };
        let cur = Some((2, 4));
        let mut quads = Vec::new();
        match effect {
            CursorDependent::Rainbow => {
                ws.cursor_rainbow.tick(
                    cur,
                    now,
                    1.0,
                    true,
                    true,
                    geom,
                    &crate::cursor_rainbow::RainbowConfig {
                        enabled: true,
                        intensity: 1.0,
                        blinking: true,
                    },
                    &mut quads,
                );
                assert!(ws.cursor_rainbow.is_active());
            }
            CursorDependent::Droplet => {
                ws.cursor_droplet.tick(
                    cur,
                    now,
                    1.0,
                    geom,
                    &crate::cursor_droplet::DropletConfig {
                        enabled: true,
                        intensity: 1.0,
                    },
                    &mut quads,
                );
                assert!(ws.cursor_droplet.is_active());
            }
            CursorDependent::BeamRod => {
                ws.cursor_beamrod.tick(
                    cur,
                    now,
                    1.0,
                    geom,
                    &crate::cursor_beam::BeamRodConfig {
                        enabled: true,
                        intensity: 1.0,
                        color: 0x00FF_44CC,
                        haze: 0x0022_1144,
                        bar: false,
                        shimmer: false,
                    },
                    &mut quads,
                );
                assert!(ws.cursor_beamrod.is_active());
            }
            CursorDependent::Fireball => {
                ws.cursor_fireball.tick(
                    cur,
                    now,
                    1.0,
                    geom,
                    &crate::cursor_fireball::FireballConfig {
                        enabled: true,
                        intensity: 1.0,
                    },
                    &mut quads,
                );
                assert!(ws.cursor_fireball.is_active());
            }
            CursorDependent::Comet => {
                ws.cursor_comet.tick(
                    cur,
                    now,
                    1.0,
                    geom,
                    &crate::cursor_comet::CometConfig {
                        enabled: true,
                        intensity: 1.0,
                        color: 0x0044_CCFF,
                        accent: 0x00DD_FFFF,
                    },
                    &mut quads,
                );
                assert!(ws.cursor_comet.is_active());
            }
            CursorDependent::Phaser => {
                ws.cursor_phaser.tick(
                    cur,
                    now,
                    0.25,
                    1.0,
                    true,
                    geom,
                    &crate::cursor_phaser::PhaserConfig {
                        enabled: true,
                        intensity: 1.0,
                    },
                    &mut quads,
                );
                assert!(ws.cursor_phaser.is_active());
            }
            CursorDependent::Trail => {
                let cfg = crate::cursor_trail::TrailConfig {
                    enabled: true,
                    duration: Duration::from_secs(1),
                    max_len: 12,
                    color: 0x00FF_44CC,
                    intensity: 0.0,
                    warmth: 0.0,
                };
                let mut cells = Vec::new();
                ws.cursor_trail.tick(Some((2, 1)), now, &cfg, &mut cells);
                ws.cursor_trail.tick(cur, now, &cfg, &mut cells);
                assert!(ws.cursor_trail.is_active());
            }
            CursorDependent::Cat => {
                ws.cursor_cat
                    .on_collect(now, aterm_effects::kitty_registry::KittyLook::default());
                assert!(ws.cursor_cat.is_active());
            }
            CursorDependent::WordDecoration => {
                panic!("word decorations need the terminal snapshot helper")
            }
        }
    }

    fn activate_word_decoration(app: &mut App, now: Instant) {
        let wid = crate::WindowId(0);
        let terminal = app
            .front_terminal(wid)
            .expect("terminal fixture")
            .term
            .clone();
        {
            let ws = app.windows.get_mut(&wid).expect("window 0");
            ws.pending_deco_birth = Some(now);
            let mut term = crate::term_lock(&terminal);
            term.process(b"\r\nhello kitty friend");
            term.cell_frame_into(&mut ws.input_scratch, ws.rows as usize, ws.cols as usize);
        }
        app.splice_word_decorations_for_test(wid, now);
        assert!(app.windows[&wid].word_decos.is_active(now));
    }

    fn activate_dependent(app: &mut App, effect: CursorDependent, now: Instant) {
        if matches!(effect, CursorDependent::WordDecoration) {
            activate_word_decoration(app, now);
        } else {
            activate_non_word_dependent(
                app.windows.get_mut(&crate::WindowId(0)).expect("window 0"),
                effect,
                now,
            );
        }
    }

    fn activate_all_terminal_dependents(app: &mut App, now: Instant) {
        for effect in CURSOR_DEPENDENTS {
            activate_dependent(app, effect, now);
        }
        let ws = app.windows.get_mut(&crate::WindowId(0)).expect("window 0");
        ws.next_trail_tick = Some(now + Duration::from_millis(16));
        ws.last_trail_fire = Some(now);
        ws.last_effect_pump_at = Some(now);
        assert!(ws.cursor_fx_active(now, true));
        assert!(ws.terminal_effect_frame_active(now, true));
    }

    fn assert_native_effect_scheduler_parked(app: &App, now: Instant) {
        let ws = &app.windows[&crate::WindowId(0)];
        assert!(
            ws.front_terminal().is_none(),
            "native front owns no terminal"
        );
        // These latches deliberately survive the switch. The canonical-front
        // gate, not destructive state loss, is what makes them scheduler-inert.
        assert!(ws.cursor_rainbow.is_active());
        assert!(ws.cursor_droplet.is_active());
        assert!(ws.cursor_beamrod.is_active());
        assert!(ws.cursor_fireball.is_active());
        assert!(ws.cursor_comet.is_active());
        assert!(ws.cursor_phaser.is_active());
        assert!(ws.cursor_cat.is_active());
        assert!(ws.word_decos.is_active(now));
        assert!(!ws.cursor_fx_active(now, true));
        assert!(!ws.cursor_fx_active(now, false));
        assert!(!ws.terminal_effect_frame_active(now, true));
        assert_eq!(ws.static_cursor_cat_deadline(now, false), None);
        assert_eq!(ws.next_trail_tick, None);
        assert_eq!(ws.last_trail_fire, None);
        assert_eq!(ws.last_effect_pump_at, None);
    }

    #[test]
    fn every_cursor_dependent_uses_the_shared_frame_cadence_set() {
        for effect in CURSOR_DEPENDENTS {
            let mut app = App::headless_for_test();
            let now = Instant::now();
            activate_dependent(&mut app, effect, now);
            let ws = app.windows.get_mut(&crate::WindowId(0)).expect("window 0");
            if matches!(effect, CursorDependent::WordDecoration) {
                // The feline word can also report a collectible. Keep this arm
                // isolated so omitting WordDecorations from the shared set could
                // not be masked by the cat predicate.
                ws.cursor_cat = crate::nyan_cursor::CursorCat::default();
            }
            let dependents = ws.cursor_dependents_need_frame_cadence(now, true);
            assert!(dependents, "{effect:?} must request frame cadence");
            assert_eq!(
                ws.cursor_fx_active(now, true),
                ws.cursor_glow.is_active() || dependents,
                "activity/cadence equality drifted for {effect:?}"
            );
            assert!(
                ws.terminal_effect_frame_active(now, true),
                "{effect:?} must reach the real scheduler predicate"
            );
        }
    }

    #[test]
    fn same_terminal_sync_preserves_live_effects_and_deadline() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        let now = Instant::now();
        activate_dependent(&mut app, CursorDependent::Trail, now);
        let deadline = now + Duration::from_millis(16);
        app.windows.get_mut(&wid).expect("window 0").next_trail_tick = Some(deadline);

        let front = app.windows[&wid].front_content;
        app.sync_window(wid);

        let ws = &app.windows[&wid];
        assert_eq!(
            ws.front_content, front,
            "ordinary sync kept the same terminal"
        );
        assert!(
            ws.cursor_trail.is_active(),
            "ordinary sync must not wipe live effects"
        );
        assert!(ws.cursor_fx_active(now, true));
        assert_eq!(
            ws.next_trail_tick,
            Some(deadline),
            "ordinary terminal bookkeeping preserves the armed cadence"
        );
    }

    #[test]
    fn settings_and_about_native_routes_park_terminal_effect_scheduler() {
        for route in [
            crate::native_settings::SettingsRoute::Home,
            crate::native_settings::SettingsRoute::About,
        ] {
            let mut app = App::headless_for_test();
            let now = Instant::now();
            activate_all_terminal_dependents(&mut app, now);
            assert!(app.open_settings_tab(route), "open native route {route:?}");
            assert_native_effect_scheduler_parked(&app, now);
        }
    }

    #[test]
    fn generic_native_tab_parks_terminal_effect_scheduler() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        let now = Instant::now();
        activate_all_terminal_dependents(&mut app, now);
        app.install_new_native_tab(
            wid,
            crate::native_app::NativeApp::Recovery(crate::native_app::RecoveryApp {
                restore_tag: "scheduler-test".to_string(),
                reason: "native cadence regression".to_string(),
                metadata: String::new(),
                capability: None,
            }),
            crate::native_app::AppViewState::Recovery(
                crate::native_app::RecoveryViewState::default(),
            ),
            crate::tab_model::TabPresentation {
                title: "Native".to_string(),
                icon: Some(crate::tab_model::TabIconKind::Recovery),
                indicators: crate::tab_model::TabIndicators::default(),
                closable: true,
                tooltip: None,
            },
        )
        .expect("install generic native tab");
        assert_native_effect_scheduler_parked(&app, now);
    }

    fn emitted_settings_patch_base(app: &mut App) -> u64 {
        let (instance, view) = app
            .active_native_view(crate::WindowId(0))
            .expect("active Settings view");
        let outcome = app
            .native_runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new(format!(
                        "settings/set/{}",
                        crate::prefs::EDIT_COPY_ON_SELECT
                    )),
                    value: Some(SemanticInput::Bool(false)),
                }),
            )
            .expect("Settings action dispatches");
        outcome
            .effects
            .iter()
            .find_map(|effect| match effect {
                AppEffect::ConfigPatch { patch, .. } => Some(patch.base_revision),
                _ => None,
            })
            .expect("Settings emits a config patch")
    }

    #[test]
    fn settings_reopen_after_external_change_uses_the_snapshot_revision() {
        let mut app = App::headless_for_test();
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::KeyboardInput));
        assert!(app.close_settings_tabs());

        // Advance the service while there is no Settings view/controller to
        // receive ConfigChanged. The reopened view observes this exact value,
        // so its first edit must not pretend to have been authored at revision 1.
        app.sync_native_config_external("copy_on_select = true\n".to_string())
            .expect("valid external config");
        let revision = app.native_config_service.snapshot().revision;
        assert!(revision > 1);

        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::KeyboardInput));
        assert_eq!(emitted_settings_patch_base(&mut app), revision);
    }

    #[test]
    fn native_settings_tab_reuses_identity_and_never_fabricates_a_terminal() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        let (terminal_tabs, terminal_layouts, sessions) = {
            let ws = app.windows.get(&wid).unwrap();
            (ws.tabs.count, ws.layouts.len(), app.pool.sessions.len())
        };

        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Appearance));
        let first_tab = app
            .windows
            .get(&wid)
            .and_then(|ws| ws.tab_set.active_id())
            .expect("Settings tab");
        assert!(app.settings_tab_open());
        assert_eq!(app.windows.get(&wid).unwrap().tabs.count, terminal_tabs);
        assert_eq!(
            app.windows.get(&wid).unwrap().layouts.len(),
            terminal_layouts
        );
        assert_eq!(
            app.pool.sessions.len(),
            sessions,
            "native Settings creates no Session"
        );

        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
        assert_eq!(
            app.windows.get(&wid).unwrap().tab_set.active_id(),
            Some(first_tab),
            "route changes reuse the same Settings presentation"
        );

        assert!(app.close_settings_tabs());
        assert!(!app.settings_tab_open());
        assert_eq!(app.windows.get(&wid).unwrap().tabs.count, terminal_tabs);
        assert_eq!(
            app.windows.get(&wid).unwrap().layouts.len(),
            terminal_layouts
        );
        assert_eq!(app.pool.sessions.len(), sessions);
    }

    #[test]
    fn control_route_open_invalidates_and_rebuilds_the_full_native_frame() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
        assert!(app.prepare_native_input_scratch(wid));
        let (about_fp, about_pixels) = app
            .windows
            .get(&wid)
            .and_then(|window| window.settings_card.as_ref())
            .map(|card| (card.fp, card.rgba.clone()))
            .expect("About materializes one complete native tray");

        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Appearance));
        assert!(
            app.windows
                .get(&wid)
                .is_some_and(|window| window.settings_card.is_none()),
            "the typed repaint effect discards the previous route raster"
        );
        assert!(app.prepare_native_input_scratch(wid));
        let (appearance_fp, appearance_pixels) = app
            .windows
            .get(&wid)
            .and_then(|window| window.settings_card.as_ref())
            .map(|card| (card.fp, card.rgba.clone()))
            .expect("Appearance rematerializes one complete native tray");
        assert_ne!(about_fp, appearance_fp);
        assert_ne!(about_pixels, appearance_pixels);

        let compiled = app.compiled_native_ui(wid).unwrap();
        assert!(
            compiled
                .semantic(&crate::native_ui::UiKey::new(
                    "settings/page-heading/appearance"
                ))
                .is_some()
        );
        assert!(
            compiled
                .semantic(&crate::native_ui::UiKey::new("about/hero"))
                .is_none(),
            "the rebuilt frame has no stale About subtree"
        );
    }

    #[test]
    fn compatibility_section_control_navigates_the_native_view() {
        let mut app = App::headless_for_test();
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        app.settings_show_section(crate::prefs::Section::Security)
            .expect("open native Settings route");
        let (_, view) = app
            .active_native_view(crate::WindowId(0))
            .expect("active Settings view");
        let crate::native_app::AppViewState::Settings(state) =
            app.native_runtime.view_state(view).expect("view state")
        else {
            panic!("Settings view kind");
        };
        assert_eq!(state.route, crate::native_settings::SettingsRoute::Security);
        assert!(app.native_settings_legacy_state().is_some());
    }

    #[test]
    fn menu_find_focuses_native_settings_search_not_parked_terminal_find() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        app.find_requested();
        let (_, view) = app.active_native_view(wid).expect("active Settings view");
        let crate::native_app::AppViewState::Settings(state) =
            app.native_runtime.view_state(view).expect("Settings state")
        else {
            panic!("Settings view kind");
        };
        assert_eq!(
            state.common.last_focus.as_ref().map(|key| key.as_str()),
            Some("settings/search")
        );
        assert!(
            app.windows.get(&wid).is_some_and(|ws| ws.search.is_none()),
            "the parked terminal never enters find mode"
        );
    }

    /// The wheel's seed hex on the front window's open settings, or panics.
    fn wheel_hex(app: &App) -> String {
        app.front()
            .and_then(|ws| ws.settings())
            .and_then(|s| s.wheel.as_ref())
            .map(|w| w.hex.clone())
            .expect("colour wheel open")
    }

    /// REGRESSION (audit — the settings-v2 headline defect): opening the colour
    /// wheel on an UNSET Color row seeds from the live theme's colour FOR THAT
    /// KEY (design §7) — the wheel on an unset Background opens ≈ `theme.bg`,
    /// never the accent (`theme.cursor`). The old one-size accent fallback made
    /// the preview instantly re-tint the whole mock cursor-green and a bare ↵
    /// persist `background = "#50FA7B"`.
    #[test]
    fn wheel_on_unset_background_seeds_theme_bg_not_accent() {
        let mut app = App::headless_for_test();
        app.settings_enter();
        let idx = app
            .front()
            .and_then(|ws| ws.settings())
            .and_then(|s| {
                s.fields
                    .iter()
                    .position(|f| f.key == crate::prefs::EDIT_BACKGROUND)
            })
            .expect("background row");
        assert_eq!(
            app.front()
                .and_then(|ws| ws.settings())
                .and_then(|s| s.fields[idx].seed.clone()),
            None,
            "background is unset in the default config (the fallback fires)"
        );
        app.settings_select(idx);
        app.settings_wheel_open();
        let hex = wheel_hex(&app);
        assert_eq!(
            hex,
            canonical_hex(u32_rgb(app.theme.bg)),
            "the unset Background row seeds the LIVE theme bg"
        );
        assert_ne!(
            hex,
            canonical_hex(u32_rgb(app.theme.cursor)),
            "…not the accent (bg and cursor are distinct in the default theme)"
        );
    }

    /// The per-key fallback covers every colour row: an unset Foreground seeds
    /// `theme.fg` (only cursor_color may legitimately equal the accent).
    #[test]
    fn wheel_on_unset_foreground_seeds_theme_fg() {
        let mut app = App::headless_for_test();
        app.settings_enter();
        let idx = app
            .front()
            .and_then(|ws| ws.settings())
            .and_then(|s| {
                s.fields
                    .iter()
                    .position(|f| f.key == crate::prefs::EDIT_FOREGROUND)
            })
            .expect("foreground row");
        app.settings_select(idx);
        app.settings_wheel_open();
        assert_eq!(wheel_hex(&app), canonical_hex(u32_rgb(app.theme.fg)));
    }
}
