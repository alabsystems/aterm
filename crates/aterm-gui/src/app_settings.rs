// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native Settings-tab host glue: singleton view discovery, open/focus/close, stable
//! route navigation, and compatibility control projections. The lower half retains
//! the former overlay-input adapter as test scaffolding around [`crate::settings`];
//! production cannot construct its `Overlay::Settings` variant.

#[cfg(test)]
use crate::Wake;
use crate::{App, WindowState};

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
    /// Enumerate every live Settings presentation in `wid`, including leaves
    /// that are not currently focused. Generic split/restore operations may
    /// place Settings beside a terminal or another native app, so focus is a
    /// presentation detail rather than an identity test.
    fn settings_tabs_in_window(
        &self,
        wid: crate::WindowId,
    ) -> Vec<(
        usize,
        crate::tab_model::TabId,
        crate::native_app::AppInstanceId,
        crate::tab_model::ViewId,
    )> {
        let Some(window) = self.windows.get(&wid) else {
            return Vec::new();
        };
        window
            .tab_set
            .tabs()
            .iter()
            .enumerate()
            .flat_map(|(index, tab)| {
                tab.root.leaves().into_iter().filter_map(move |view| {
                    let crate::tab_model::View::Native(native) =
                        self.view_store.get(view).copied()?
                    else {
                        return None;
                    };
                    (self.native_runtime.app(native.instance)?.kind()
                        == crate::native_app::AppKind::Settings)
                        .then_some((index, tab.id, native.instance, view))
                })
            })
            .collect()
    }

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
        let window = self.windows.get(&wid)?;
        let active = window.tab_set.active_id();
        let focused = window.tab_set.active().map(|tab| tab.focus);
        self.settings_tabs_in_window(wid)
            .into_iter()
            .min_by_key(|(index, tab, _, view)| {
                (
                    usize::from(Some(*tab) != active),
                    usize::from(Some(*view) != focused),
                    *index,
                    view.get(),
                )
            })
    }

    /// Resolve one Settings view in `wid`. The ordering mirrors
    /// [`Self::settings_tab_in_window`]: an active/focused presentation wins, then
    /// stable tab/view order.
    fn native_settings_view_target_in_window(
        &self,
        wid: crate::WindowId,
    ) -> Option<(
        crate::native_app::AppInstanceId,
        crate::tab_model::ViewId,
        &crate::native_settings::SettingsViewState,
    )> {
        let window = self.windows.get(&wid)?;
        let active = window.tab_set.active_id();
        let focused = window.tab_set.active().map(|tab| tab.focus);
        let mut targets = self.settings_tabs_in_window(wid);
        targets.sort_by_key(|(index, tab, _, view)| {
            (
                usize::from(Some(*tab) != active),
                usize::from(Some(*view) != focused),
                *index,
                view.get(),
            )
        });
        targets.into_iter().find_map(|(_, _, instance, view)| {
            let crate::native_app::AppViewState::Settings(state) =
                self.native_runtime.view_state(view)?
            else {
                return None;
            };
            Some((instance, view, state.as_ref()))
        })
    }

    fn native_settings_view_target_matching(
        &self,
    ) -> Option<(
        crate::WindowId,
        crate::native_app::AppInstanceId,
        crate::tab_model::ViewId,
        &crate::native_settings::SettingsViewState,
    )> {
        let front = self.frontmost_window.and_then(|wid| {
            self.native_settings_view_target_in_window(wid)
                .map(|(instance, view, state)| (wid, instance, view, state))
        });
        if front.is_some() {
            return front;
        }
        let mut windows = self.windows.keys().copied().collect::<Vec<_>>();
        windows.sort_unstable_by_key(|wid| wid.0);
        windows.into_iter().find_map(|wid| {
            self.native_settings_view_target_in_window(wid)
                .map(|(instance, view, state)| (wid, instance, view, state))
        })
    }

    /// Resolve a Settings presentation from the front window's active tab only.
    /// Compatibility `window about|update` captures that exact window/tab, so its
    /// semantic twin must never fall back to an inactive tab or another window.
    fn native_settings_front_view_target_matching(
        &self,
        route: Option<crate::native_settings::SettingsRoute>,
    ) -> Option<(
        crate::WindowId,
        crate::native_app::AppInstanceId,
        crate::tab_model::ViewId,
        &crate::native_settings::SettingsViewState,
    )> {
        let wid = self.frontmost_window?;
        let tab = self.windows.get(&wid)?.tab_set.active()?;
        let mut views = tab.root.leaves();
        views.sort_unstable_by_key(|view| (usize::from(*view != tab.focus), view.get()));
        views.into_iter().find_map(|view| {
            let crate::tab_model::View::Native(native) = self.view_store.get(view).copied()? else {
                return None;
            };
            if self.native_runtime.app(native.instance)?.kind()
                != crate::native_app::AppKind::Settings
            {
                return None;
            }
            let crate::native_app::AppViewState::Settings(state) =
                self.native_runtime.view_state(view)?
            else {
                return None;
            };
            route
                .is_none_or(|expected| state.route == expected)
                .then_some((wid, native.instance, view, state.as_ref()))
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
        // This runs once per event-loop park (`retry_title_observations`), and
        // the common case is that no Settings view exists anywhere. Settings is
        // a process-singleton controller, so NO live instance proves no window
        // presents it: the per-window/per-tab walk below (one `leaves()` heap
        // allocation per tab) and the health projection (up to four `String`
        // clones plus two schedule scans) are then both pure waste.
        //
        // `instance_by_kind` scans the handful of live native apps and
        // allocates nothing, and it is conservative in the safe direction: an
        // instance with no attached view merely falls through to the walk,
        // which then finds no target exactly as it does today. It is NOT
        // `settings_tab_open()` — that helper re-runs this very walk, so
        // guarding with it would double the cost in the case that matters.
        if self
            .native_runtime
            .instance_by_kind(crate::native_app::AppKind::Settings)
            .is_none()
        {
            return;
        }
        let targets = self
            .windows
            .keys()
            .copied()
            .filter_map(|wid| {
                self.settings_tab_in_window(wid)
                    .map(|(_, _, _, view)| (wid, view))
            })
            .collect::<Vec<_>>();
        if targets.is_empty() {
            return;
        }
        // Hoisted below the target search: with no target the whole body was
        // already a no-op, so the projection's allocations bought nothing.
        // `title_summary_health()` is `&self` and side-effect-free, which is
        // what makes the reorder behaviour-identical.
        let health = self.title_summary_health();
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

    /// Locate the native Settings presentation targeted by compatibility control
    /// verbs. Prefer the front window, then any other live presentation of the
    /// process-singleton controller. Returning the real view state and stable IDs
    /// lets inspection compile the same semantic tree as paint instead of reviving
    /// the retired overlay's parallel projection.
    pub(crate) fn native_settings_view_target(
        &self,
    ) -> Option<(
        crate::WindowId,
        crate::native_app::AppInstanceId,
        crate::tab_model::ViewId,
        &crate::native_settings::SettingsViewState,
    )> {
        self.native_settings_view_target_matching()
    }

    /// Locate whichever Settings view is actually visible in the front window's active
    /// tab. Route-specific compatibility aliases use this for honest mismatch reports;
    /// unlike [`Self::native_settings_view_target`], it never falls back process-wide.
    pub(crate) fn native_settings_front_view_target(
        &self,
    ) -> Option<(
        crate::WindowId,
        crate::native_app::AppInstanceId,
        crate::tab_model::ViewId,
        &crate::native_settings::SettingsViewState,
    )> {
        self.native_settings_front_view_target_matching(None)
    }

    /// Locate a live presentation of one exact Settings route for a compatibility
    /// alias in the front window's active tab. This is deliberately distinct from the
    /// generic `controls prefs` target: `controls about` and `controls update` must either
    /// inspect the page photographed by `window about|update` or report that it is not
    /// frontmost, never serialize a background or parallel retired model.
    pub(crate) fn native_settings_route_view_target(
        &self,
        route: crate::native_settings::SettingsRoute,
    ) -> Option<(
        crate::WindowId,
        crate::native_app::AppInstanceId,
        crate::tab_model::ViewId,
        &crate::native_settings::SettingsViewState,
    )> {
        self.native_settings_front_view_target_matching(Some(route))
    }

    /// Legacy overlay-model access retained only for the old model-level tests.
    /// Production control inspection uses [`Self::native_settings_view_target`]
    /// and the native compiled semantic tree.
    #[cfg(test)]
    pub(crate) fn native_settings_legacy_state(&self) -> Option<&crate::settings::SettingsState> {
        self.native_settings_view_target()
            .map(|(_, _, _, state)| &state.legacy)
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
    /// and before legacy overlay model tests serialize. Cheap at rest: one revision
    /// compare, no clone. The repaint rides `RepaintKey::settings_fp`, which
    /// folds the revision only while the Kitty Log category is active.
    #[cfg(test)]
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
                conn: None,
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
            // A Settings leaf may share its tab with a focused terminal. An
            // explicit Settings action must reveal and target the exact native
            // presentation instead of merely activating its containing tab.
            let focused = ws
                .tab_set
                .active_mut()
                .is_some_and(|tab| tab.set_focus(view));
            if !focused {
                return false;
            }
            // A last-tab close parks the logical window behind `pending_close`
            // until its main-loop caller can escalate with an ActiveEventLoop.
            // An explicit reopen can race that escalation (or recover a window
            // left behind by an older caller), so installing/focusing a real tab
            // must cancel the stale empty-window teardown before another wake
            // observes it.
            ws.pending_close = false;
            ws.last_present = None;
        }
        // Returning from Manual's separate config Editor (or another native
        // tab) to an existing Settings tab must publish the canonical front
        // content before the exact-view stale-target guard resolves the action.
        self.resync_active_or_window(wid);
        self.sync_settings_title_summary_health();
        if route == crate::native_settings::SettingsRoute::SoftwareUpdate {
            self.acknowledge_native_update_attention();
            // The screen paints the reducer's snapshot, and the reducer only learns
            // ledger changes on a reconcile. Opening the screen IS the moment the
            // user wants the current verdict (a check that failed persistently
            // stages nothing, so nothing else would have imported it).
            self.request_native_update_reconcile(
                crate::app_native::NativeUpdateReconcilePurpose::Refresh,
            );
        }
        // Seed + refresh the shared Packages projection whenever Settings
        // surfaces: publication is memory-only (the controller starts honest
        // "unobserved"), and the status collection runs on a worker thread —
        // never a status.toml parse on the event loop.
        self.publish_native_packages_state();
        self.start_native_packages_refresh();
        let action_id = if route == crate::native_settings::SettingsRoute::Manual {
            crate::native_ui::ActionId::new("settings/manual/open")
        } else {
            crate::native_ui::ActionId::new(format!("settings/route{}", route.path()))
        };
        let action = crate::native_app::AppEvent::Action(crate::native_app::ActionInvocation {
            id: action_id,
            value: None,
        });
        // Reuse the exact human/semantic-action host path.  The old direct
        // runtime dispatch threw away `InvalidateOwnPresentation` and
        // `RepaintSelf`, leaving the model on the requested route while the
        // retained app-render tray still held the previous page.
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
        // Snapshot stable identities once. A blocked draft is attempted once
        // and remains focused with its recovery UI; successful removals cannot
        // make this loop rediscover and spin on that same blocked view.
        let targets: Vec<(
            crate::WindowId,
            crate::tab_model::TabId,
            crate::tab_model::ViewId,
        )> = self
            .windows
            .keys()
            .copied()
            .flat_map(|wid| {
                self.settings_tabs_in_window(wid)
                    .into_iter()
                    .map(move |(_, tab, _, view)| (wid, tab, view))
            })
            .collect();
        let mut removed = false;
        for (wid, tab, view) in targets {
            let focused = self.windows.get_mut(&wid).is_some_and(|ws| {
                if !ws.tab_set.switch_to(tab) {
                    return false;
                }
                ws.tab_set
                    .active_mut()
                    .is_some_and(|active| active.set_focus(view))
            });
            if !focused {
                continue;
            }
            self.resync_active_or_window(wid);
            let split = self
                .windows
                .get(&wid)
                .and_then(|ws| ws.tab_set.active())
                .is_some_and(|active| active.root.len() > 1);
            let result = if split {
                self.close_focused_mixed_leaf(wid)
            } else {
                self.close_active_native_tab(wid)
            };
            match result {
                Ok(()) => removed = true,
                Err(_) => {
                    // Keep the refusing Settings view selected so its retained
                    // draft banner and exact close-recovery palette remain
                    // visible to the control caller.
                    self.sync_window(wid);
                }
            }
        }
        removed
    }

    /// Apply the compatibility `settings [open|close]` request while keeping
    /// process singleton identity separate from per-window presentation. An
    /// explicit open always addresses the current front window; toggle and
    /// close retain their historical process-wide meaning.
    pub(crate) fn apply_settings_open_request(&mut self, open: Option<bool>) -> bool {
        match open {
            Some(true) => {
                let _ = self.open_settings_tab(crate::native_settings::SettingsRoute::Home);
            }
            Some(false) => {
                self.close_settings_tabs();
            }
            None if self.settings_tab_open() => {
                self.close_settings_tabs();
            }
            None => {
                let _ = self.open_settings_tab(crate::native_settings::SettingsRoute::Home);
            }
        }
        self.settings_tab_open()
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
    #[cfg(test)]
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
    #[cfg(test)]
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
        #[cfg(test)]
        self.sync_settings_kitty_log();
        self.settings_repaint_front();
    }

    /// →/Tab/↵ from the sidebar: give the content pane keyboard focus.
    #[cfg(test)]
    pub(crate) fn settings_focus_content(&mut self) {
        if let Some(s) = self.settings_host_mut().and_then(|ws| ws.settings_mut()) {
            s.focus_content();
        }
        self.settings_repaint_front();
    }

    /// Esc/Tab from the content pane: give the sidebar keyboard focus.
    #[cfg(test)]
    pub(crate) fn settings_focus_sidebar(&mut self) {
        if let Some(s) = self.settings_host_mut().and_then(|ws| ws.settings_mut()) {
            s.focus_sidebar();
        }
        self.settings_repaint_front();
    }

    /// Tab/⇧Tab: toggle keyboard focus between the two panes (design §6).
    #[cfg(test)]
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
    #[cfg(test)]
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
    #[cfg(test)]
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
    #[cfg(test)]
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
    #[cfg(test)]
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

    /// `settings section <name>` (socket): navigate the OPEN native Settings tab to the
    /// parsed stable route. The control parser accepts visible labels and compatibility
    /// aliases before this main-thread boundary; no retired category is representable
    /// here. ERR when no Settings view is up.
    pub(crate) fn settings_show_route(
        &mut self,
        route: crate::native_settings::SettingsRoute,
    ) -> Result<(), String> {
        if !self.settings_tab_open() {
            return Err("settings not open (use: settings open)".to_string());
        }
        self.open_settings_tab(route)
            .then_some(())
            .ok_or_else(|| "could not focus the native Settings tab".to_string())
    }

    /// Landing ↵: a non-empty suggestion box SENDS; an empty one is Get started
    /// (the hero's one Enter affordance stays useful either way).
    #[cfg(test)]
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
    #[cfg(test)]
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
    #[cfg(test)]
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
    /// the key as REMOVED (`None`) through the retired overlay's test-only persistence
    /// seam, then optimistically clear the row's seed so it shows the
    /// default this frame.
    #[cfg(test)]
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
                #[cfg(test)]
                if let Some(proxy) = self.proxy.as_ref() {
                    let _ = proxy.send_event(Wake::ConfigReload);
                }
                format!("reset: {key} = (default)")
            }
            crate::prefs::SaveOutcome::Unchanged => format!("{key}: already default"),
            crate::prefs::SaveOutcome::Conflict { message, .. } => {
                format!("reset conflict: {message}; reload aterm.toml before retrying")
            }
            crate::prefs::SaveOutcome::PublishedUnverified { message, .. } => format!(
                "reset publication unverified: {message}; reload aterm.toml before retrying"
            ),
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
    /// a footer status,
    /// and an optimistic seed update so the row reflects the value THIS frame (the
    /// authoritative rebuild follows when the reload lands; both produce the same seed).
    pub(crate) fn settings_commit_value(
        &mut self,
        key: &'static str,
        val: Option<String>,
    ) -> String {
        // THE TYPING-SOUND AUDITION rides every commit gesture on its row
        // (before the "unchanged" early return below — see the fn).
        self.settings_commit_audition(key, val.as_deref());
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

        // Persist through the shared pure, atomic, format-preserving writer. A clone
        // keeps `val` for the optimistic snapshot below.
        let outcome = crate::prefs::save_prefs_edits(&[(key, val.clone())]);
        let persisted = matches!(outcome, crate::prefs::SaveOutcome::Saved);
        let status = match &outcome {
            crate::prefs::SaveOutcome::Saved => {
                // Retired overlay test seam: request its explicit local resample.
                #[cfg(test)]
                if let Some(proxy) = self.proxy.as_ref() {
                    let _ = proxy.send_event(Wake::ConfigReload);
                }
                format!("saved: {key} = {}", val.as_deref().unwrap_or(""))
            }
            crate::prefs::SaveOutcome::Unchanged => format!("{key}: unchanged"),
            crate::prefs::SaveOutcome::Conflict { message, .. } => {
                format!("save conflict: {message}; reload aterm.toml before retrying")
            }
            crate::prefs::SaveOutcome::PublishedUnverified { message, .. } => {
                format!("publication unverified: {message}; reload aterm.toml before retrying")
            }
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
                s.status = Some(status.clone());
            }
            if let Some(w) = &ws.os_window {
                w.request_redraw();
            }
        }
        self.overlay_a11y_update();
        status
    }

    /// The commit-time half of the typing-sound audition: every commit
    /// gesture on the "Typing sound" row — Enter on the highlighted entry, a
    /// popup pick, a ←/→ step — auditions the committed voice
    /// UNCONDITIONALLY, the "unchanged" case included: Enter on the current
    /// voice is "play it again", and scrubbing the list with ←/→ auditions
    /// each voice as it goes by. A cleared value is the default, `auto`; an
    /// unparseable one (a preserved custom entry) is what the runtime would
    /// play for it — `auto` too. Split from [`Self::settings_commit_value`]
    /// so the hook is provable without touching the on-disk config.
    fn settings_commit_audition(&mut self, key: &str, val: Option<&str>) {
        if key != crate::prefs::EDIT_TRAIL_SOUND_STYLE {
            return;
        }
        let voice = val
            .and_then(aterm_effects::trail_sound::SoundVoice::parse)
            .unwrap_or_default();
        self.audition_typing_sound(voice);
    }

    /// The reload-time half of the typing-sound audition, decided BEFORE a
    /// config swap against the latch: a native-window pick or a hand edit
    /// that CHANGES the voice returns it for one audition after the swap;
    /// the in-app row already auditioned at commit time and latched the same
    /// voice, so its own reload is silent; startup never reaches the swap.
    /// Pure over `(next config, latch)` so the dedupe law is provable.
    pub(crate) fn typing_sound_to_audition_on_swap(
        &self,
        next: &crate::app_config::Config,
    ) -> Option<aterm_effects::trail_sound::SoundVoice> {
        let next_voice = next.trail_sound_voice();
        (next_voice != self.typing_sound_auditioned).then_some(next_voice)
    }

    /// THE TYPING-SOUND AUDITION — "picking a voice plays one keystroke of
    /// it, so you choose by ear." One [`aterm_effects::trail_sound::SoundKind::Typed`]
    /// cue in `voice`, exactly as the loudness ladder measures a keystroke
    /// (`mix_meter`'s stance: pan 0, heat 0.5, hue 0, `Tone::Technical`, no
    /// bed) at the user's volume, riding the current trail look (which only
    /// matters under `auto`, where the audition IS today's sound). Gated by
    /// the SAME predicate the key-time click uses
    /// ([`crate::app_input::keystroke_click_audible`]): a live audio host, the
    /// "Music effects" master, a non-zero volume, and serious mode allowing
    /// terminal sound — so the preview can never speak where a keystroke
    /// could not. Latches `typing_sound_auditioned` either way, so the config
    /// reload that follows an in-app commit does not play the voice twice.
    pub(crate) fn audition_typing_sound(&mut self, voice: aterm_effects::trail_sound::SoundVoice) {
        use aterm_effects::trail_sound::{SoundEvent, SoundGesture, SoundKind};
        self.typing_sound_auditioned = voice;
        let volume = self.config.trail_sound_volume();
        let audible = crate::app_input::keystroke_click_audible(
            self.trail_audio.is_live(),
            self.config.trail_sounds_or_default(),
            volume,
            self.serious_mode_policy()
                .allows(crate::motion::SeriousEffect::TerminalSound),
            false,
        );
        if !audible {
            return;
        }
        self.trail_audio.push(SoundEvent {
            style: self.glow_style(),
            voice,
            kind: SoundGesture::Trail(SoundKind::Typed),
            pan: 0.0,
            heat: 0.5,
            hue: 0.0,
            gain: volume,
            tone: aterm_effects::tone::Tone::Technical,
            bed: false,
        });
    }

    /// The live [`crate::settings::SettingsGeom`] of the front window's settings card —
    /// the SAME cell/font/row numbers `splice_settings_panel` paints with, consumed by
    /// the menu placement + mouse hit-test paths. `None` when the overlay is closed.
    pub(crate) fn settings_geom_front(&self) -> Option<crate::settings::SettingsGeom> {
        let wid = self.frontmost_window?;
        self.windows.get(&wid)?.settings()?;
        self.overlay_coordinate_transform(wid)
            .map(|transform| transform.geom)
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
    #[cfg(test)]
    pub(crate) fn settings_menu_move(&mut self, delta: isize) {
        let visible = self.settings_menu_visible();
        if let Some(s) = self.settings_host_mut().and_then(|ws| ws.settings_mut()) {
            s.menu_move(delta, visible);
        }
        self.settings_repaint_front();
    }

    /// Jump the popup menu highlight to the next option starting with `c`.
    #[cfg(test)]
    pub(crate) fn settings_menu_jump(&mut self, c: char) {
        let visible = self.settings_menu_visible();
        if let Some(s) = self.settings_host_mut().and_then(|ws| ws.settings_mut()) {
            s.menu_jump(c, visible);
        }
        self.settings_repaint_front();
    }

    /// Wheel-scroll the popup menu's option window by `delta` rows.
    #[cfg(test)]
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
    #[cfg(test)]
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
    #[cfg(test)]
    pub(crate) fn settings_wheel_focus_next(&mut self) {
        if let Some(s) = self.settings_host_mut().and_then(|ws| ws.settings_mut()) {
            s.wheel_focus_next();
        }
        self.settings_repaint_front();
    }

    /// Arrow-key adjust of the wheel's focused sub-control (`big` = Shift): hue/
    /// saturation on the disk, brightness on the value slider (design §7).
    #[cfg(test)]
    pub(crate) fn settings_wheel_arrow(&mut self, dx: f32, dy: f32, big: bool) {
        if let Some(s) = self.settings_host_mut().and_then(|ws| ws.settings_mut()) {
            s.wheel_arrow(dx, dy, big);
        }
        self.settings_repaint_front();
    }

    /// Type into the wheel's hex readout (no-op unless the hex field has focus).
    #[cfg(test)]
    pub(crate) fn settings_wheel_hex_push(&mut self, c: char) {
        if let Some(s) = self.settings_host_mut().and_then(|ws| ws.settings_mut()) {
            s.wheel_hex_push(c);
        }
        self.settings_repaint_front();
    }

    /// Delete the last hex character (no-op unless the hex field has focus).
    #[cfg(test)]
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
    #[cfg(test)]
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
    #[cfg(test)]
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
    #[cfg(test)]
    pub(crate) fn settings_edit_push(&mut self, c: char) {
        if let Some(s) = self.settings_host_mut().and_then(|ws| ws.settings_mut()) {
            s.edit_push(c);
        }
        self.settings_repaint_front();
    }

    /// Delete the last character of the in-panel edit buffer.
    #[cfg(test)]
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
    /// seam. A rejected value (bad number) sets a status message
    /// and STAYS in edit mode so the user can fix it; a clean commit leaves edit mode.
    #[cfg(test)]
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
            crate::prefs::SaveOutcome::Conflict { .. }
            | crate::prefs::SaveOutcome::PublishedUnverified { .. }
            | crate::prefs::SaveOutcome::Error(_) => (false, false),
        };
        let status = match &outcome {
            crate::prefs::SaveOutcome::Saved => {
                #[cfg(test)]
                if let Some(proxy) = self.proxy.as_ref() {
                    let _ = proxy.send_event(Wake::ConfigReload);
                }
                match val.as_deref() {
                    Some(v) => format!("saved: {key} = {v}"),
                    None => format!("saved: {key} = (default)"),
                }
            }
            crate::prefs::SaveOutcome::Unchanged => format!("{key}: unchanged"),
            crate::prefs::SaveOutcome::Conflict { message, .. } => {
                format!("conflict for {key}: {message}; reload aterm.toml before retrying")
            }
            crate::prefs::SaveOutcome::PublishedUnverified { message, .. } => format!(
                "publication for {key} is unverified: {message}; reload aterm.toml before retrying"
            ),
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
        #[cfg(a11y_tree)]
        if let Some(wid) = self.frontmost_window {
            self.push_a11y_tree(wid);
        }
    }

    /// Build the accessibility tree for window `wid`'s OPEN overlay (Settings / About /
    /// Palette / Update) — or an empty root when nothing is open — and hand it to that
    /// window's AccessKit adapter. The tree fans out through [`crate::overlay::OverlayModel::a11y`]
    /// off the SAME model the pixels + `controls` verb read, so which surface is live routes
    /// itself (a missing variant is a compile error, not a Settings-only fallback to empty).
    ///
    /// PERF: bails before building ANYTHING unless an OS a11y client has actually attached
    /// ([`crate::WindowState::a11y_active`]). The whole body below is thrown away by
    /// `update_if_active` while the adapter is inactive — it just used to be thrown away
    /// *after* walking every visible cell into a string, on every present. Attachment always
    /// arrives as `InitialTreeRequested`, which raises the latch and then publishes here, so
    /// the first tree a screen reader sees is unchanged.
    #[cfg(a11y_tree)]
    pub(crate) fn push_a11y_tree(&mut self, wid: crate::WindowId) {
        if self
            .windows
            .get(&wid)
            .is_none_or(|ws| ws.a11y.is_none() || !ws.a11y_active)
        {
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
        // The grid's on-screen geometry and the tab strip it sits under. Both are read
        // from state the frame ALREADY computed (`win_cell_size`/`win_pad_top` metrics,
        // and the very `tab_segments` the mouse hit-tests against), so this adds no
        // layout work — and, like everything else here, it is only reached once an OS
        // a11y client has attached.
        let grid_geometry = grid_snap
            .is_some()
            .then(|| self.grid_a11y_geometry(wid))
            .flatten();
        let grid_tabs = if grid_snap.is_some() {
            self.grid_a11y_tabs(wid)
        } else {
            Vec::new()
        };
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
                    Some(snap) => (
                        crate::accesskit_tree::grid_tree(snap, grid_geometry, &grid_tabs),
                        None,
                    ),
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

    /// Where window `wid`'s visible grid (and the tab strip above it) landed in the client
    /// area, in PHYSICAL pixels — the space `accesskit_winit` reports root window bounds
    /// in. Straight from the window's own metrics, the same numbers the frame was laid out
    /// with; `None` for an unknown window or a degenerate cell size, in which case the
    /// tree simply omits geometry (the Text interface is unaffected).
    #[cfg(a11y_tree)]
    fn grid_a11y_geometry(
        &self,
        wid: crate::WindowId,
    ) -> Option<crate::accesskit_tree::GridGeometry> {
        // An unknown window would silently fall back to the SHARED backend metrics, which
        // is another window's geometry — fail closed instead.
        if !self.windows.contains_key(&wid) {
            return None;
        }
        let (cw, ch) = self.win_cell_size(wid);
        if cw == 0 || ch == 0 {
            return None;
        }
        let strip_rows = usize::from(self.tab_strip_rows);
        // The strip band starts below the top pad + chrome headroom; the GRID starts below
        // the band — exactly `native_content_origin_y`, and exactly where `grid_snapshot`
        // re-bases the cursor to.
        let strip_y = self.win_pad_top(wid).saturating_add(self.win_head(wid));
        Some(crate::accesskit_tree::GridGeometry {
            origin_x: self.win_pad(wid) as f64,
            origin_y: self.native_content_origin_y(wid) as f64,
            cell_w: cw as f64,
            cell_h: ch as f64,
            strip_y: strip_y as f64,
            strip_h: (strip_rows * ch) as f64,
        })
    }

    /// The in-grid tab strip of window `wid` as publishable a11y items, or empty when
    /// there is nothing a screen reader should be told about.
    ///
    /// Empty unless the strip is actually ON SCREEN (`tab_strip_rows > 0`) and holds at
    /// least two tabs: a solo strip is painted as the window TITLE, not a switcher (see
    /// `TabSegment::solo`), so publishing a one-item tab list would announce a control
    /// that is not there. Labels come from the window's `strip_titles_scratch` and column
    /// spans from its cached `tab_segments` — the very buffers the last paint filled and
    /// the mouse hit-tests against, so the announced tab and the clickable tab are the
    /// same one by construction.
    #[cfg(a11y_tree)]
    fn grid_a11y_tabs(&self, wid: crate::WindowId) -> Vec<crate::accesskit_tree::GridTab> {
        if self.tab_strip_rows == 0 {
            return Vec::new();
        }
        let Some(ws) = self.windows.get(&wid) else {
            return Vec::new();
        };
        if ws.tab_set.len() < 2 {
            return Vec::new();
        }
        let active = ws.tab_set.active_index();
        ws.tab_segments
            .iter()
            .filter_map(|segment| {
                let crate::tab_bar::TabHit::Select(index) = segment.kind else {
                    return None;
                };
                if segment.solo {
                    return None;
                }
                Some(crate::accesskit_tree::GridTab {
                    index,
                    title: ws
                        .strip_titles_scratch
                        .get(index)
                        .cloned()
                        .unwrap_or_else(|| "aterm".to_string()),
                    selected: active == Some(index),
                    start_col: segment.start_col,
                    end_col: segment.end_col,
                })
            })
            .collect()
    }

    /// P2: handle an event from a window's AccessKit adapter (delivered as
    /// `Wake::Accessibility`): the OS a11y client requesting the initial tree, an action
    /// request from a screen reader, or deactivation.
    #[cfg(a11y_tree)]
    pub(crate) fn on_accessibility_event(&mut self, event: accesskit_winit::Event) {
        use accesskit_winit::WindowEvent as AkWindowEvent;
        let Some(wid) = self.winit_to_window.get(&event.window_id).copied() else {
            return;
        };
        match event.window_event {
            // The OS a11y client is initialising — hand it the live overlay's tree. This is
            // the ONLY edge that says "someone is actually listening": raise the latch first
            // so the publish below (and every later present) is allowed to build a tree.
            AkWindowEvent::InitialTreeRequested => {
                if let Some(ws) = self.windows.get_mut(&wid) {
                    ws.a11y_active = true;
                }
                self.push_a11y_tree(wid);
            }
            // A screen reader activated/focused a control → drive the live overlay's model.
            AkWindowEvent::ActionRequested(req) => self.on_accessibility_action(wid, req),
            // The client detached. Drop the latch so presents stop building trees nobody
            // reads; a later re-attach re-fires `InitialTreeRequested` (the platform adapter
            // returns to its inactive state, so it re-runs the activation handler). Never
            // fires on macOS, whose winit adapter installs no deactivation handler.
            AkWindowEvent::AccessibilityDeactivated => {
                if let Some(ws) = self.windows.get_mut(&wid) {
                    ws.a11y_active = false;
                }
            }
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
    #[cfg(a11y_tree)]
    fn on_accessibility_action(&mut self, wid: crate::WindowId, req: accesskit::ActionRequest) {
        use crate::overlay::OverlayKind;
        // Screen-reader actions bypass keyboard/pointer ingress. Even an
        // ignored/stale request is a newer external-input boundary and must
        // close the cursor-move licence before any local early return.
        self.clear_move_license(wid);
        let kind = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.overlay.as_ref())
            .map(crate::overlay::Overlay::kind);
        let Some(kind) = kind else {
            // No overlay: the published tree is either a native tab app's projection or
            // the terminal grid. Only the grid tree mints tab ids, and it mints them in a
            // range no other builder uses, so decoding one here cannot steal a native
            // app's request. Focus and Click both mean "show me this tab" — a tab strip
            // has no separate select-vs-activate step.
            if matches!(
                req.action,
                accesskit::Action::Click | accesskit::Action::Focus
            ) && let Some(index) = crate::accesskit_tree::tab_index_for(req.target_node)
            {
                self.switch_tab_in(wid, index);
                if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
                    w.request_redraw();
                }
                // Republish synchronously: a screen reader must hear the new selection
                // even if the visual frame is coalesced away.
                self.push_a11y_tree(wid);
                return;
            }
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
            OverlayKind::ConnCard => {
                if req.action != accesskit::Action::Click {
                    return;
                }
                let Some(hit) = crate::conn_card::ConnCardState::a11y_hit(req.target_node) else {
                    return;
                };
                match hit {
                    crate::conn_card::ConnCardHit::Confirm => self.conn_card_confirm(wid),
                    crate::conn_card::ConnCardHit::Cancel => self.conn_card_exit(wid),
                    // The chooser nodes cycle their own value (a screen reader
                    // clicks the row it hears).
                    crate::conn_card::ConnCardHit::Direction(_)
                    | crate::conn_card::ConnCardHit::Kind(_) => {
                        let row = match hit {
                            crate::conn_card::ConnCardHit::Direction(_) => {
                                crate::conn_card::CardRow::Direction
                            }
                            _ => crate::conn_card::CardRow::Kind,
                        };
                        if let Some(c) = self
                            .windows
                            .get_mut(&wid)
                            .and_then(|ws| ws.conn_card_mut())
                        {
                            c.cycle_row(row, 1);
                        }
                        self.overlay_a11y_update();
                        if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref())
                        {
                            w.request_redraw();
                        }
                    }
                }
            }
            // The session picker — the palette's id scheme verbatim: Focus
            // moves the cursor, Click chooses; stale-epoch nodes decode to
            // nothing.
            OverlayKind::SessionPicker => {
                let Some(idx) = self
                    .windows
                    .get(&wid)
                    .and_then(|ws| ws.session_picker())
                    .and_then(|picker| picker.a11y_filtered_index(req.target_node))
                else {
                    return; // root / list container carries no row action
                };
                match req.action {
                    accesskit::Action::Focus => {
                        if let Some(p) = self
                            .windows
                            .get_mut(&wid)
                            .and_then(|ws| ws.session_picker_mut())
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
                        if let Some(p) = self
                            .windows
                            .get_mut(&wid)
                            .and_then(|ws| ws.session_picker_mut())
                        {
                            p.select(idx);
                        }
                        self.session_picker_activate(wid);
                    }
                    _ => {}
                }
            }
            // The connection map — the palette's id scheme over its chip/arrow
            // items: Focus moves the cursor, Click selects then activates
            // (chip raises; a flow runs the inline confirm two-step, so a
            // screen-reader Click can never disconnect unconfirmed).
            OverlayKind::ConnectionMap => {
                let Some(idx) = self
                    .windows
                    .get(&wid)
                    .and_then(|ws| ws.connection_map())
                    .and_then(|map| map.a11y_item_index(req.target_node))
                else {
                    return; // root / list container carries no row action
                };
                match req.action {
                    accesskit::Action::Focus => {
                        if let Some(m) = self
                            .windows
                            .get_mut(&wid)
                            .and_then(|ws| ws.connection_map_mut())
                        {
                            m.select(idx);
                        }
                        self.overlay_a11y_update();
                        if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref())
                        {
                            w.request_redraw();
                        }
                    }
                    accesskit::Action::Click => {
                        if let Some(m) = self
                            .windows
                            .get_mut(&wid)
                            .and_then(|ws| ws.connection_map_mut())
                        {
                            m.select(idx);
                        }
                        self.connection_map_activate(wid);
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
    use aterm_effects::cursor_glow::GlowStyle;
    use aterm_effects::trail_sound::{SoundGesture, SoundKind, SoundVoice};

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
                // The scripted three-cell preview hop is a deliberate generic
                // gesture, not a one-glyph echo (which correctly takes the
                // large-delta re-anchor path).
                ws.cursor_trail.note_synthetic_move(now);
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
        // The chrome-decoration lane is a scheduler input too, and unlike the
        // cursor engines it is not focus-gated — so arm it here or the native
        // boundary below is only asserted for the effects that never needed it.
        ws.note_deco_animating(now);
        assert!(ws.cursor_fx_active(now, true));
        assert!(ws.terminal_effect_frame_active(now, true));
        assert!(ws.deco_anim_frame_active(now));
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
        // The decoration latch is the one input with no focus gate of its own,
        // so the canonical-front boundary is the ONLY thing keeping Robi from
        // pacing a native tab. `park_terminal_effect_scheduler` drops it on the
        // way through; a terminal that comes back refreshes it on frame one.
        assert!(!ws.deco_anim_frame_active(now));
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
                ws.cursor_cat = crate::kitty_cursor::CursorCat::default();
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
                conn: None,
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
        app.native_config_service
            .replace_external("copy_on_select = true\n".to_string())
            .expect("valid external config");
        let revision = app.native_config_service.snapshot().revision;
        assert!(revision > 1);

        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::KeyboardInput));
        assert_eq!(emitted_settings_patch_base(&mut app), revision);
    }

    #[test]
    fn native_settings_tab_reuses_identity_and_never_fabricates_a_terminal() {
        const CHILD: &str = "ATERM_NATIVE_SETTINGS_TAB_IDENTITY_CHILD";
        const ROOT: &str = "ATERM_NATIVE_SETTINGS_TAB_IDENTITY_ROOT";
        const EXACT: &str = concat!(
            "app_settings::tests::",
            "native_settings_tab_reuses_identity_and_never_fabricates_a_terminal"
        );
        if std::env::var_os(CHILD).is_none() {
            let root = std::env::temp_dir().join(format!(
                "aterm-native-settings-tab-identity-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", EXACT, "--nocapture"])
                .env(CHILD, "1")
                .env(ROOT, &root)
                .env("XDG_CONFIG_HOME", &root)
                .env("RUST_TEST_THREADS", "1")
                .status()
                .expect("launch isolated native Settings-tab identity test");
            let _ = std::fs::remove_dir_all(root);
            assert!(status.success());
            return;
        }

        let root = std::path::PathBuf::from(std::env::var_os(ROOT).unwrap());
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        let (terminal_tabs, terminal_layouts, sessions) = {
            let ws = app.windows.get(&wid).unwrap();
            (ws.tabs.count, ws.layouts.len(), app.pool.sessions.len())
        };

        let mut first_tab = None;
        for route in crate::native_settings::SettingsRoute::ALL
            .into_iter()
            .filter(|route| *route != crate::native_settings::SettingsRoute::Manual)
        {
            assert!(app.open_settings_tab(route), "open native route {route:?}");
            let active_tab = app
                .windows
                .get(&wid)
                .and_then(|ws| ws.tab_set.active_id())
                .expect("Settings tab");
            let canonical_tab = *first_tab.get_or_insert(active_tab);
            assert_eq!(
                active_tab, canonical_tab,
                "every pane reuses the same Settings presentation"
            );
            let (instance, _) = app
                .active_native_view(wid)
                .expect("active native Settings view");
            assert_eq!(
                app.native_runtime.app(instance).map(|native| native.kind()),
                Some(crate::native_app::AppKind::Settings),
                "{route:?} stays inside the native Settings app"
            );
            assert!(app.prepare_native_input_scratch(wid), "render {route:?}");
            let ws = app.windows.get(&wid).unwrap();
            assert!(ws.front_terminal().is_none(), "{route:?} owns no PTY");
            assert!(ws.settings_card.is_some(), "{route:?} compiled native UI");
            assert_eq!(ws.tabs.count, terminal_tabs);
            assert_eq!(ws.layouts.len(), terminal_layouts);
            assert_eq!(
                app.pool.sessions.len(),
                sessions,
                "native Settings creates no Session"
            );
        }

        // Manual deliberately opens the canonical config Editor instead of
        // pretending to be another settings pane. Returning from it must still
        // recover the original Settings presentation.
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Manual));
        assert!(
            root.join("aterm/aterm.toml").is_file(),
            "Manual must resolve only the isolated config file"
        );
        let (manual_instance, _) = app.active_native_view(wid).expect("Manual editor tab");
        assert_eq!(
            app.native_runtime
                .app(manual_instance)
                .map(|native| native.kind()),
            Some(crate::native_app::AppKind::Editor)
        );
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        assert_eq!(
            app.windows.get(&wid).and_then(|ws| ws.tab_set.active_id()),
            first_tab,
            "returning from Manual reuses the canonical Settings tab"
        );
        assert!(app.settings_tab_open());

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
    fn settings_presentation_split_discovery_focus_and_close_preserve_terminal_siblings() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (instance, first_settings) = app.active_native_view(wid).expect("Settings view");
        let second_settings = app
            .split_active_with_native(
                wid,
                crate::tab_model::SplitAxis::Horizontal,
                instance,
                crate::native_app::AppViewState::Settings(Box::new(
                    crate::native_settings::SettingsViewState::new(&app.config),
                )),
            )
            .expect("second Settings presentation");
        let (terminal_session, terminal_view) =
            app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Vertical);
        assert_eq!(app.focused_session_id(wid), Some(terminal_session));
        assert_eq!(app.settings_tabs_in_window(wid).len(), 2);
        assert!(
            app.settings_tab_open(),
            "nonfocused Settings leaves remain open"
        );

        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
        let (_, focused_settings) = app.active_native_view(wid).expect("Settings focused");
        assert!(
            [first_settings, second_settings].contains(&focused_settings),
            "open focuses an existing Settings leaf"
        );
        assert_eq!(
            app.settings_tabs_in_window(wid).len(),
            2,
            "open never duplicates a visible nonfocused presentation"
        );

        assert!(app.close_settings_tabs());
        assert!(!app.settings_tab_open());
        assert!(matches!(
            app.view_store.get(terminal_view),
            Some(crate::tab_model::View::Terminal(terminal))
                if terminal.session == terminal_session
        ));
        assert!(app.pool.get(terminal_session).is_some());
        assert!(
            app.windows[&wid]
                .tab_set
                .tabs()
                .iter()
                .any(|tab| tab.root.contains(terminal_view))
        );
        assert!(app.structural_invariants_ok());
    }

    #[test]
    fn settings_presentation_introspection_uses_nonfocused_split_leaf_viewport() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (instance, settings) = app.active_native_view(wid).expect("Settings view");
        let (_, terminal) =
            app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
        assert!(app.active_native_view(wid).is_none());
        assert_eq!(
            app.windows[&wid].tab_set.active().map(|tab| tab.focus),
            Some(terminal)
        );

        let full = app.native_ui_viewport(wid).expect("window viewport");
        let exact = app
            .native_ui_viewport_for(wid, settings)
            .expect("Settings leaf viewport");
        assert!(
            exact.width < full.width,
            "split leaf is narrower than its window"
        );
        let expected = app
            .compiled_native_ui_for(wid, instance, settings, exact)
            .expect("compile exact Settings leaf")
            .controls_lines();
        let emitted = app
            .read_aux_controls(crate::app_introspect::AuxTarget::Prefs)
            .into_iter()
            .filter(|line| line.starts_with("ui "))
            .collect::<Vec<_>>();
        assert_eq!(emitted, expected);
    }

    #[test]
    fn settings_presentation_explicit_open_is_idempotent_per_front_window() {
        let mut app = App::headless_for_test();
        let first = crate::WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let first_instance = app.settings_tabs_in_window(first)[0].2;

        let session = app.next_session_id;
        let second = app.insert_logical_window(crate::stub_session(session), 24, 80);
        assert_eq!(app.frontmost_window, Some(second));
        assert!(app.settings_tabs_in_window(second).is_empty());
        assert!(
            app.settings_tab_open(),
            "the first window still presents Settings"
        );

        assert!(app.apply_settings_open_request(Some(true)));
        let second_presentations = app.settings_tabs_in_window(second);
        assert_eq!(second_presentations.len(), 1);
        assert_eq!(second_presentations[0].2, first_instance);
        assert_eq!(
            app.windows
                .keys()
                .copied()
                .flat_map(|wid| app.settings_tabs_in_window(wid))
                .filter(|(_, _, instance, _)| *instance == first_instance)
                .count(),
            2,
            "both windows share one Settings controller"
        );

        assert!(app.apply_settings_open_request(Some(true)));
        assert_eq!(
            app.settings_tabs_in_window(second).len(),
            1,
            "a repeated explicit open reuses the requesting window's view"
        );
    }

    #[test]
    fn settings_last_tab_close_arms_teardown_and_reopen_cancels_the_stale_flag() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        assert_eq!(app.windows[&wid].tab_set.len(), 2);

        // Retire the original terminal tab so this becomes the supported
        // native-only shape produced by recursive restore/detach. `false` means
        // the closed terminal was not the window's final canonical tab.
        assert!(!app.close_tab_at(wid, 0));
        assert_eq!(app.windows[&wid].tab_set.len(), 1);
        assert!(app.settings_tab_open());

        assert!(app.close_settings_tabs());
        assert!(!app.settings_tab_open());
        assert!(app.windows[&wid].tab_set.is_empty());
        assert!(
            app.windows[&wid].pending_close,
            "the main-loop Settings wake must escalate this empty window"
        );

        // If an explicit open wins the race before escalation, the new tab is
        // authoritative and the old pending-close edge must not later destroy it.
        assert!(app.apply_settings_open_request(Some(true)));
        assert_eq!(app.windows[&wid].tab_set.len(), 1);
        assert!(app.settings_tab_open());
        assert!(
            !app.windows[&wid].pending_close,
            "reopening a real Settings tab cancels stale empty-window teardown"
        );
        assert!(app.structural_invariants_ok());
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
                .semantic(&crate::native_ui::UiKey::new("settings/preview/appearance"))
                .is_some(),
            "the compact frame presents Appearance's exact live preview even when its redundant heading is shed"
        );
        assert!(
            compiled
                .semantic(&crate::native_ui::UiKey::new("about/hero"))
                .is_none(),
            "the rebuilt frame has no stale About subtree"
        );
    }

    #[test]
    fn section_control_navigates_the_typed_native_route() {
        let mut app = App::headless_for_test();
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        for route in [
            crate::native_settings::SettingsRoute::CursorMotion,
            crate::native_settings::SettingsRoute::TextFonts,
            crate::native_settings::SettingsRoute::Security,
        ] {
            app.settings_show_route(route)
                .expect("open native Settings route");
            let (_, view) = app
                .active_native_view(crate::WindowId(0))
                .expect("active Settings view");
            let crate::native_app::AppViewState::Settings(state) =
                app.native_runtime.view_state(view).expect("view state")
            else {
                panic!("Settings view kind");
            };
            assert_eq!(state.route, route);
        }
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

    // -- the typing-sound audition ----------------------------------------

    /// One captured cue's `(voice, kind, gain)`.
    fn captured_typed(app: &mut App) -> Vec<(SoundVoice, f32)> {
        app.trail_audio
            .take_captured_for_test()
            .into_iter()
            .map(|e| {
                assert_eq!(
                    e.kind,
                    SoundGesture::Trail(SoundKind::Typed),
                    "an audition is one keystroke, nothing else"
                );
                assert!(!e.bed, "the audition never feeds the bed");
                assert_eq!(e.pan, 0.0);
                (e.voice, e.gain)
            })
            .collect()
    }

    fn app_with_capture() -> App {
        let mut app = App::headless_for_test();
        app.trail_audio = crate::trail_audio::TrailAudio::capturing_for_test();
        app
    }

    /// Committing the "Typing sound" row plays EXACTLY ONE keystroke of the
    /// committed voice at the user's volume — for a picker pick, and again
    /// for the same value (Enter on the current entry is "play it again"),
    /// and again per ←/→ step; a cleared value auditions `auto`. Other rows
    /// audition nothing.
    #[test]
    fn committing_the_typing_sound_row_auditions_one_keystroke() {
        let mut app = app_with_capture();
        app.config.trail_sound_volume = Some(0.25);
        app.settings_commit_audition(crate::prefs::EDIT_TRAIL_SOUND_STYLE, Some("glass bell"));
        assert_eq!(
            captured_typed(&mut app),
            vec![(SoundVoice::Of(GlowStyle::RainbowKitty), 0.25)]
        );
        // "Play it again": the same value auditions again.
        app.settings_commit_audition(crate::prefs::EDIT_TRAIL_SOUND_STYLE, Some("glass bell"));
        assert_eq!(captured_typed(&mut app).len(), 1);
        // Scrubbing: each step auditions the voice it lands on (aliases too).
        for (raw, voice) in [
            ("typewriter", SoundVoice::Typewriter),
            ("Marimba", SoundVoice::Marimba),
            ("water", SoundVoice::Of(GlowStyle::Water)),
            ("felt", SoundVoice::Felt),
        ] {
            app.settings_commit_audition(crate::prefs::EDIT_TRAIL_SOUND_STYLE, Some(raw));
            assert_eq!(captured_typed(&mut app), vec![(voice, 0.25)], "{raw}");
        }
        // Cleared = the default = auto; a preserved custom entry plays what
        // the runtime would play for it, auto.
        app.settings_commit_audition(crate::prefs::EDIT_TRAIL_SOUND_STYLE, None);
        assert_eq!(captured_typed(&mut app), vec![(SoundVoice::Style, 0.25)]);
        app.settings_commit_audition(crate::prefs::EDIT_TRAIL_SOUND_STYLE, Some("kazoo"));
        assert_eq!(captured_typed(&mut app), vec![(SoundVoice::Style, 0.25)]);
        // Any other row is silent.
        app.settings_commit_audition(crate::prefs::EDIT_TRAIL_SOUND_VOLUME, Some("0.5"));
        app.settings_commit_audition(crate::prefs::EDIT_CURSOR_TRAIL_STYLE, Some("water"));
        assert!(captured_typed(&mut app).is_empty());
    }

    /// The audition is gated exactly like a keystroke: the "Music effects"
    /// master off, a zero volume, or serious mode ⇒ nothing is pushed (the
    /// latch still moves, so no later reload plays it either).
    #[test]
    fn the_audition_is_gated_like_a_keystroke() {
        let mut app = app_with_capture();
        app.config.trail_sounds = Some(false);
        app.audition_typing_sound(SoundVoice::Marimba);
        assert!(captured_typed(&mut app).is_empty(), "master off");
        assert_eq!(app.typing_sound_auditioned, SoundVoice::Marimba);
        app.config.trail_sounds = Some(true);
        app.config.trail_sound_volume = Some(0.0);
        app.audition_typing_sound(SoundVoice::Felt);
        assert!(captured_typed(&mut app).is_empty(), "muted");
        app.config.trail_sound_volume = Some(0.4);
        app.serious_mode = true;
        app.audition_typing_sound(SoundVoice::Typewriter);
        assert!(captured_typed(&mut app).is_empty(), "serious mode");
        app.serious_mode = false;
        app.audition_typing_sound(SoundVoice::Typewriter);
        assert_eq!(
            captured_typed(&mut app),
            vec![(SoundVoice::Typewriter, 0.4)]
        );
        // An inert host (no audio backend / headless): silent.
        app.trail_audio = crate::trail_audio::TrailAudio::new(false);
        app.audition_typing_sound(SoundVoice::Mech);
        assert_eq!(app.typing_sound_auditioned, SoundVoice::Mech);
    }

    /// The reload path auditions a CHANGED voice once, an unchanged one
    /// never, and the in-app commit followed by its own reload plays ONE
    /// keystroke in total (the latch dedupes).
    #[test]
    fn a_config_swap_auditions_only_a_changed_voice_and_never_twice() {
        let mut app = app_with_capture();
        let mut next = app.config.clone();
        // Startup voice (auto) → an unchanged reload: silent.
        assert_eq!(app.typing_sound_to_audition_on_swap(&next), None);
        // A hand edit / native pick that CHANGES the voice: one audition.
        next.trail_sound_style = Some("droplet".into());
        assert_eq!(
            app.typing_sound_to_audition_on_swap(&next),
            Some(SoundVoice::Of(GlowStyle::Water))
        );
        // …and the swap latches it, so re-applying the same config is silent.
        app.audition_typing_sound(SoundVoice::Of(GlowStyle::Water));
        assert_eq!(captured_typed(&mut app).len(), 1);
        assert_eq!(app.typing_sound_to_audition_on_swap(&next), None);
        // Aliases dedupe by VOICE, not spelling: `water` is still droplet.
        next.trail_sound_style = Some(" Water ".into());
        assert_eq!(app.typing_sound_to_audition_on_swap(&next), None);
        // THE PAIR: in-app commit (auditions + latches) then its own reload.
        app.settings_commit_audition(crate::prefs::EDIT_TRAIL_SOUND_STYLE, Some("marimba"));
        assert_eq!(captured_typed(&mut app), vec![(SoundVoice::Marimba, 0.4)]);
        next.trail_sound_style = Some("marimba".into());
        assert_eq!(
            app.typing_sound_to_audition_on_swap(&next),
            None,
            "the commit's own reload must not play the voice a second time"
        );
        // Clearing the key back to auto from a file edit auditions auto once.
        next.trail_sound_style = None;
        assert_eq!(
            app.typing_sound_to_audition_on_swap(&next),
            Some(SoundVoice::Style)
        );
    }

    /// PERF/CONTRACT: `push_a11y_tree` builds the whole visible-screen tree eagerly and only
    /// then offers it to `update_if_active`, so the adapter's own "is anyone listening?" test
    /// arrives far too late to save the work. The window's `a11y_active` latch is what moves
    /// that test to the front — it must start DOWN (the adapter exists for every OS window,
    /// attached or not), rise on the attach edge, and fall again on detach.
    #[cfg(a11y_tree)]
    #[test]
    fn accessibility_publish_latches_on_attach_and_clears_on_detach() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        let winit_id = winit::window::WindowId::from(1u64);
        app.winit_to_window.insert(winit_id, wid);

        assert!(
            !app.windows[&wid].a11y_active,
            "no OS a11y client has attached, so presents must not build a tree"
        );
        app.on_accessibility_event(accesskit_winit::Event {
            window_id: winit_id,
            window_event: accesskit_winit::WindowEvent::InitialTreeRequested,
        });
        assert!(
            app.windows[&wid].a11y_active,
            "InitialTreeRequested is the attach edge — publishing must resume"
        );
        app.on_accessibility_event(accesskit_winit::Event {
            window_id: winit_id,
            window_event: accesskit_winit::WindowEvent::AccessibilityDeactivated,
        });
        assert!(
            !app.windows[&wid].a11y_active,
            "the client detached; a re-attach re-fires InitialTreeRequested"
        );
    }

    /// Accessibility actions bypass keyboard and pointer ingress. Even a stale
    /// or unsupported request is a newer external-input boundary, so it must
    /// retire an older exact cursor candidate before native/overlay routing can
    /// return without consuming it.
    #[cfg(a11y_tree)]
    #[test]
    fn accessibility_action_supersedes_pending_cursor_candidate() {
        use accesskit::{Action, ActionRequest, NodeId, TreeId};

        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        app.config.cursor_trail = Some(true);
        app.config.cursor_trail_style = Some("lumen".to_string());
        let glow_cfg = app.glow_config();
        let trail_cfg = crate::cursor_trail::TrailConfig {
            enabled: true,
            duration: Duration::from_millis(300),
            max_len: 24,
            color: 0x0050_FA7B,
            intensity: 0.5,
            warmth: 0.0,
        };
        let geom = crate::cursor_glow::Geom {
            cw: 8,
            ch: 16,
            rows: 6,
            cols: 40,
            origin_x: 0,
            origin_y: 0,
            win_w: 320,
            win_h: 96,
            head: 0,
        };
        let now = Instant::now();
        let mut glow = Vec::new();
        let mut trail = Vec::new();
        {
            let ws = app.windows.get_mut(&wid).unwrap();
            ws.cursor_glow
                .tick(Some((0, 0)), now, &glow_cfg, geom, &mut glow);
            ws.cursor_trail
                .tick(Some((0, 0)), now, &trail_cfg, &mut trail);
            ws.cursor_glow.note_synthetic_move(now);
            ws.cursor_trail.note_synthetic_move(now);
            // The synthetic note licenses the move it is about to make: the
            // engines' own predicate, since `move_candidate_pending` retired
            // with the admission-scoreboard rework.
            assert!(ws.cursor_glow.move_licensed(now));
            assert!(ws.cursor_trail.move_licensed(now));
        }

        let winit_id = winit::window::WindowId::from(17u64);
        app.winit_to_window.insert(winit_id, wid);
        app.on_accessibility_event(accesskit_winit::Event {
            window_id: winit_id,
            window_event: accesskit_winit::WindowEvent::ActionRequested(ActionRequest {
                action: Action::Focus,
                target_tree: TreeId::ROOT,
                target_node: NodeId(999_999),
                data: None,
            }),
        });
        let ws = app.windows.get_mut(&wid).unwrap();
        // …and an a11y action that targets no live node revokes that licence,
        // so the move it would have decorated draws nothing.
        assert!(!ws.cursor_glow.move_licensed(now));
        assert!(!ws.cursor_trail.move_licensed(now));
        let moved = now + Duration::from_millis(1);
        assert_eq!(
            ws.cursor_glow
                .tick(Some((0, 1)), moved, &glow_cfg, geom, &mut glow),
            0
        );
        assert_eq!(
            ws.cursor_trail
                .tick(Some((0, 1)), moved, &trail_cfg, &mut trail),
            0
        );
        assert!(glow.is_empty() && trail.is_empty());
    }
}
