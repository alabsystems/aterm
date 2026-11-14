// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native Settings-tab host glue: singleton view discovery, open/focus/close, route navigation, and control projections.

use crate::App;

#[cfg(test)]
thread_local! {
    pub(crate) static FONT_CATALOG_DELIVERIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(crate) static FONT_REFUSAL_COPIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(crate) static SETTINGS_TOPOLOGY_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn tccutil_presence_once() -> aterm_containment::TccutilPresence {
    static PRESENCE: std::sync::OnceLock<aterm_containment::TccutilPresence> =
        std::sync::OnceLock::new();
    *PRESENCE.get_or_init(aterm_containment::consent::tccutil_presence)
}

#[derive(Default)]
struct SettingsFonts {
    listed_for: Option<crate::native_app::AppInstanceId>,
    families: Option<std::sync::Arc<aterm_render::font_catalog::MonospaceFamilies>>,
    refusals: std::sync::Arc<Vec<crate::native_font_catalog::FontRefusal>>,
}

#[cfg(not(test))]
static SETTINGS_FONTS: std::sync::LazyLock<std::sync::Mutex<SettingsFonts>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(SettingsFonts::default()));

#[cfg(test)]
thread_local! {
    // A test's UI thread owns its process facts; no reset can clear another test's cache.
    static TEST_SETTINGS_FONTS: std::cell::RefCell<SettingsFonts> =
        std::cell::RefCell::new(SettingsFonts::default());
}

fn with_settings_fonts<R>(read: impl FnOnce(&mut SettingsFonts) -> R) -> R {
    #[cfg(test)]
    {
        TEST_SETTINGS_FONTS.with(|fonts| read(&mut fonts.borrow_mut()))
    }
    #[cfg(not(test))]
    {
        read(
            &mut SETTINGS_FONTS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }
}

/// The installed fonts the Settings font pickers were last given.
pub(crate) fn settings_font_families()
-> Option<std::sync::Arc<aterm_render::font_catalog::MonospaceFamilies>> {
    with_settings_fonts(|fonts| fonts.families.clone())
}

impl App {
    fn settings_tabs_in_window(
        &self,
        wid: crate::WindowId,
    ) -> Vec<(
        usize,
        crate::tab_model::TabId,
        crate::native_app::AppInstanceId,
        crate::tab_model::ViewId,
    )> {
        let mut views = Vec::new();
        self.visit_settings_tabs_in_window(wid, |index, tab, instance, view| {
            views.push((index, tab, instance, view));
        });
        views
    }

    /// Visit in stable tab/leaf order without materializing each tab's leaves.
    pub(crate) fn visit_settings_tabs_in_window(
        &self,
        wid: crate::WindowId,
        mut visit: impl FnMut(
            usize,
            crate::tab_model::TabId,
            crate::native_app::AppInstanceId,
            crate::tab_model::ViewId,
        ),
    ) {
        #[cfg(test)]
        SETTINGS_TOPOLOGY_VISITS.with(|n| n.set(n.get() + 1));
        let Some(window) = self.windows.get(&wid) else {
            return;
        };
        for (index, tab) in window.tab_set.tabs().iter().enumerate() {
            tab.root.visit(&mut |&view| {
                let Some(crate::tab_model::View::Native(native)) =
                    self.view_store.get(view).copied()
                else {
                    return;
                };
                if self
                    .native_runtime
                    .app(native.instance)
                    .is_some_and(|app| app.kind() == crate::native_app::AppKind::Settings)
                {
                    visit(index, tab.id, native.instance, view);
                }
            });
        }
    }

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
        let mut best_key: Option<(usize, usize, usize, u64)> = None;
        let mut best = None;
        self.visit_settings_tabs_in_window(wid, |index, tab, instance, view| {
            let key = (
                usize::from(Some(tab) != active),
                usize::from(Some(view) != focused),
                index,
                view.get(),
            );
            if best_key.is_none_or(|min| key < min) {
                best_key = Some(key);
                best = Some((index, tab, instance, view));
            }
        });
        best
    }

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
        let mut best = None;
        tab.root.visit(&mut |&view| {
            let key = (usize::from(view != tab.focus), view.get());
            if best.is_some_and(|(best_key, _)| key >= best_key) {
                return;
            }
            let Some(crate::tab_model::View::Native(native)) = self.view_store.get(view).copied()
            else {
                return;
            };
            if self
                .native_runtime
                .app(native.instance)
                .is_none_or(|app| app.kind() != crate::native_app::AppKind::Settings)
            {
                return;
            }
            let Some(crate::native_app::AppViewState::Settings(state)) =
                self.native_runtime.view_state(view)
            else {
                return;
            };
            if route.is_none_or(|expected| state.route == expected) {
                best = Some((key, (wid, native.instance, view, state.as_ref())));
            }
        });
        best.map(|(_, target)| target)
    }

    /// Whether any window presents the process-singleton native Settings app.
    pub(crate) fn settings_tab_open(&self) -> bool {
        self.windows
            .keys()
            .copied()
            .any(|wid| self.settings_tab_in_window(wid).is_some())
    }

    /// The pass before every WAIT that hands each Settings view what it shows from the App.
    pub(crate) fn sync_settings_views(&mut self) {
        self.sync_settings_consent_posture();
        self.sync_settings_font_families();
        self.sync_settings_session_runtime();
    }

    /// Copy existing in-memory journal/keeper facts into every Settings view.
    pub(crate) fn sync_settings_session_runtime(&mut self) {
        if self
            .native_runtime
            .instance_by_kind(crate::native_app::AppKind::Settings)
            .is_none()
        {
            return;
        }
        let runtime = crate::native_settings::SessionRuntime {
            desk: self.desk,
            journal: self.session_log_journal.status(),
            keeper: crate::keeper_link::status(),
        };
        let changed = self.native_runtime.deliver_settings_runtime(runtime);
        self.redraw_changed_settings_views(&changed);
    }

    fn redraw_changed_settings_views(&mut self, changed: &[crate::tab_model::ViewId]) {
        if changed.is_empty() {
            return;
        }
        let windows: Vec<_> = self.windows.keys().copied().collect();
        for wid in windows {
            let mut holds_changed = false;
            self.visit_settings_tabs_in_window(wid, |_, _, _, view| {
                holds_changed |= changed.contains(&view);
            });
            if holds_changed && let Some(window) = self.windows.get_mut(&wid) {
                window.last_present = None;
                if let Some(window) = &window.os_window {
                    window.request_redraw();
                }
            }
        }
    }

    /// Hand the installed-font list to every Settings view.
    pub(crate) fn sync_settings_font_families(&mut self) {
        let Some(instance) = self
            .native_runtime
            .instance_by_kind(crate::native_app::AppKind::Settings)
        else {
            return;
        };
        let (listed_for, families, refusals) = with_settings_fonts(|fonts| {
            (
                fonts.listed_for,
                fonts.families.clone(),
                std::sync::Arc::clone(&fonts.refusals),
            )
        });
        if listed_for != Some(instance) {
            with_settings_fonts(|fonts| fonts.listed_for = Some(instance));
            if let Some(proxy) = self.proxy.clone() {
                let _ = crate::qos::spawn_lane_thread(
                    "aterm-font-families",
                    crate::qos::Role::Background,
                    move || {
                        let families = aterm_render::font_catalog::monospace_families();
                        let _ = proxy.send_event(crate::Wake::FontFamiliesListed(families));
                    },
                );
            }
        }
        if let Some(families) = families {
            self.hand_settings_font_families(&families);
        }
        self.hand_settings_font_refusals(&refusals, false);
        self.hand_settings_drawn_faces();
    }

    fn hand_settings_drawn_faces(&mut self) {
        let changed = self
            .native_runtime
            .deliver_settings_drawn(self.backend.drawn_faces());
        self.redraw_changed_settings_views(&changed);
    }

    /// A font generation's refused rows ([`crate::native_font_catalog::FontRefusal`]).
    pub(crate) fn publish_settings_font_refusals(
        &mut self,
        refusals: Vec<crate::native_font_catalog::FontRefusal>,
    ) {
        let refusals = std::sync::Arc::new(refusals);
        with_settings_fonts(|fonts| fonts.refusals = std::sync::Arc::clone(&refusals));
        self.hand_settings_font_refusals(&refusals, true);
    }

    fn hand_settings_font_refusals(
        &mut self,
        refusals: &std::sync::Arc<Vec<crate::native_font_catalog::FontRefusal>>,
        announce: bool,
    ) {
        let changed = self
            .native_runtime
            .deliver_settings_refusals(std::sync::Arc::clone(refusals), announce);
        self.redraw_changed_settings_views(&changed);
    }

    /// The listing thread's answer ([`crate::Wake::FontFamiliesListed`]).
    pub(crate) fn publish_settings_font_families(
        &mut self,
        families: std::sync::Arc<aterm_render::font_catalog::MonospaceFamilies>,
    ) {
        with_settings_fonts(|fonts| fonts.families = Some(std::sync::Arc::clone(&families)));
        self.hand_settings_font_families(&families);
    }

    fn hand_settings_font_families(
        &mut self,
        families: &std::sync::Arc<aterm_render::font_catalog::MonospaceFamilies>,
    ) {
        let changed = self
            .native_runtime
            .deliver_settings_families(std::sync::Arc::clone(families));
        self.redraw_changed_settings_views(&changed);
    }

    /// Ask a frame of every window that holds a native leaf's raster after a chrome font
    /// landed: the leaves drawn without it are stale under their stamp
    /// ([`crate::tray_raster::chrome_coverage_epoch`]) and draw again, the rest are kept.
    pub(crate) fn redraw_native_leaves_for_landed_fonts(&self) {
        for window in self.windows.values() {
            if window
                .leaf_render_cache
                .values()
                .any(|cache| cache.native.is_some())
            {
                window.request_redraw();
            }
        }
    }

    /// Repaint every window whose Settings specimen was waiting on the font worker.
    pub(crate) fn repaint_settings_specimens(&mut self) {
        let phase_ms = u64::try_from(self.lat_epoch.elapsed().as_millis()).unwrap_or(u64::MAX);
        let windows = self.windows.keys().copied().collect::<Vec<_>>();
        for wid in windows {
            if self.invalidate_active_native_settings_preview(wid, phase_ms)
                && let Some(os_window) = self
                    .windows
                    .get(&wid)
                    .and_then(|window| window.os_window.as_ref())
            {
                os_window.request_redraw();
            }
        }
    }

    /// Publish the macOS file-access posture into every open Settings view and perform
    /// any outstanding *Ask for folder access now* press.
    pub(crate) fn sync_settings_consent_posture(&mut self) {
        if self
            .native_runtime
            .instance_by_kind(crate::native_app::AppKind::Settings)
            .is_none()
        {
            return;
        }
        let mut views = Vec::new();
        for &wid in self.windows.keys() {
            self.visit_settings_tabs_in_window(wid, |_, _, _, view| views.push((wid, view)));
        }
        let quiet = std::sync::Arc::clone(&self.owner_quiet);
        for (_, view) in &views {
            self.native_runtime
                .change_view_state(*view, |state| match state {
                    crate::native_app::AppViewState::Settings(state) => {
                        state.arm_owner_quiet(std::sync::Arc::clone(&quiet))
                    }
                    _ => false,
                });
        }
        self.drain_settings_gesture_requests(&views);
        match &self.security_posture {
            crate::security_posture::SecurityPostureSource::Absent => {}
            crate::security_posture::SecurityPostureSource::Host => {
                self.publish_security_page(views);
            }
            crate::security_posture::SecurityPostureSource::Staged { access, .. } => {
                let access = (**access).clone();
                let targets = self.security_route_views(views);
                self.publish_macos_access(
                    &targets,
                    &access,
                    crate::native_settings::ConsentGestures::for_instance(true),
                );
            }
        }
    }

    fn security_route_views(
        &self,
        views: Vec<(crate::WindowId, crate::tab_model::ViewId)>,
    ) -> Vec<(crate::WindowId, crate::tab_model::ViewId)> {
        views
            .into_iter()
            .filter(|(_, view)| {
                matches!(
                    self.native_runtime.view_state(*view),
                    Some(crate::native_app::AppViewState::Settings(state))
                        if state.route == crate::native_settings::SettingsRoute::Security
                )
            })
            .collect()
    }

    /// Take a staged posture's block off every Settings view that carries one.
    pub(crate) fn clear_macos_access(&mut self) {
        let mut views = Vec::new();
        for &wid in self.windows.keys() {
            self.visit_settings_tabs_in_window(wid, |_, _, _, view| views.push((wid, view)));
        }
        for (wid, view) in views {
            let changed = match self.native_runtime.view_state_mut(view) {
                Some(crate::native_app::AppViewState::Settings(state)) => {
                    state.clear_macos_access()
                }
                _ => false,
            };
            if changed && let Some(window) = self.windows.get_mut(&wid) {
                window.last_present = None;
                if let Some(os_window) = window.os_window.as_ref() {
                    os_window.request_redraw();
                }
            }
        }
    }

    /// How many Settings views draw the MACOS access block now.
    pub(crate) fn settings_views_drawing_macos_access(&self) -> usize {
        let mut views = Vec::new();
        for &wid in self.windows.keys() {
            self.visit_settings_tabs_in_window(wid, |_, _, _, view| views.push(view));
        }
        views
            .into_iter()
            .filter(|view| {
                matches!(
                    self.native_runtime.view_state(*view),
                    Some(crate::native_app::AppViewState::Settings(state))
                        if state.route == crate::native_settings::SettingsRoute::Security
                            && state.carries_macos_access()
                )
            })
            .count()
    }

    fn publish_security_page(&mut self, views: Vec<(crate::WindowId, crate::tab_model::ViewId)>) {
        let targets = self.security_route_views(views);
        if targets.is_empty() {
            return;
        }
        let on_screen = targets
            .iter()
            .any(|&(wid, view)| self.settings_view_on_screen(wid, view));
        let access = self.macos_access_projection(on_screen);
        let gestures = crate::native_settings::ConsentGestures::for_instance(self.headless);
        self.publish_macos_access(&targets, &access, gestures);
    }

    fn settings_view_on_screen(
        &self,
        wid: crate::WindowId,
        view: crate::tab_model::ViewId,
    ) -> bool {
        self.windows.get(&wid).is_some_and(|window| {
            !window.occluded
                && window
                    .tab_set
                    .active()
                    .is_some_and(|tab| tab.root.contains(view))
        })
    }

    fn publish_macos_access(
        &mut self,
        targets: &[(crate::WindowId, crate::tab_model::ViewId)],
        access: &crate::native_settings::MacosAccess,
        gestures: crate::native_settings::ConsentGestures,
    ) {
        for &(wid, view) in targets {
            let changed = self
                .native_runtime
                .change_view_state(view, |state| match state {
                    crate::native_app::AppViewState::Settings(state) => {
                        let armed = state.arm_consent_gestures(gestures);
                        state.replace_macos_access(access.clone()) || armed
                    }
                    crate::native_app::AppViewState::Mail(_)
                    | crate::native_app::AppViewState::About(_)
                    | crate::native_app::AppViewState::SystemState(_)
                    | crate::native_app::AppViewState::Editor(_)
                    | crate::native_app::AppViewState::Recovery(_)
                    | crate::native_app::AppViewState::Workers(_)
                    | crate::native_app::AppViewState::Tables(_)
                    | crate::native_app::AppViewState::Allocation(_) => false,
                });
            if changed && let Some(window) = self.windows.get_mut(&wid) {
                window.last_present = None;
                if let Some(os_window) = window.os_window.as_ref() {
                    os_window.request_redraw();
                }
            }
        }
    }

    fn drain_settings_gesture_requests(
        &mut self,
        views: &[(crate::WindowId, crate::tab_model::ViewId)],
    ) {
        let mut requested_by = Vec::new();
        let mut opened = false;
        let mut fda_page_up = false;
        let mut pane_dialogs = Vec::new();
        let mut retire = None;
        let mut debugger = None;
        let mut askpass_requests = Vec::new();
        let mut devtools_by = Vec::new();
        for &(wid, view) in views {
            self.native_runtime.change_view_state(view, |state| {
                let crate::native_app::AppViewState::Settings(state) = state else {
                    return false;
                };
                let mut took = false;
                if state.take_consent_warmup_request() {
                    requested_by.push((wid, view));
                    took = true;
                }
                if state.take_devtools_hand_over_request() {
                    devtools_by.push((wid, view));
                    took = true;
                }
                let open = state.take_consent_open_request();
                let fda = state.take_fda_page_opened();
                if let Some(dialog) = state.take_consent_pane_dialog() {
                    pane_dialogs.push(dialog);
                    took = true;
                }
                opened |= open;
                fda_page_up |= fda;
                took |= open || fda;
                let retiring = state.take_claimant_retire_request();
                took |= retiring.is_some();
                retire = retire.take().or(retiring);
                let change = state.take_debugger_change_request();
                took |= change.is_some();
                debugger = debugger.take().or(change);
                if let Some((off, owner)) = state.take_askpass_off_request() {
                    askpass_requests.push((wid, view, off, owner));
                    took = true;
                }
                took
            });
        }
        for dialog in pane_dialogs {
            self.adopt_access_dialog(Some(dialog), std::time::Instant::now());
        }
        #[cfg(target_os = "macos")]
        self.apply_askpass_off_requests(askpass_requests, crate::askpass::set_relay_off);
        #[cfg(not(target_os = "macos"))]
        let _ = askpass_requests;
        self.owner_quiet
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .settle_confirmed();
        if !requested_by.is_empty()
            && let Some(why) = self.begin_consent_warmup().refusal()
        {
            for (wid, view) in requested_by {
                let noted = match self.native_runtime.view_state_mut(view) {
                    Some(crate::native_app::AppViewState::Settings(state)) => {
                        state.note_warmup_refused(why)
                    }
                    _ => false,
                };
                if noted && let Some(window) = self.windows.get_mut(&wid) {
                    window.last_present = None;
                    if let Some(os_window) = window.os_window.as_ref() {
                        os_window.request_redraw();
                    }
                }
            }
        }
        if let Some(plan) = retire {
            self.claimant_retire_refusal = self.begin_claimant_retire(plan).err();
        }
        if let Some((change, answer)) = debugger {
            let refusal = self.begin_debugger_access_change(change, answer).err();
            if let Some(why) = refusal {
                aterm_log::info!(
                    "debugger access: the confirmed change started nothing: {}",
                    why.words()
                );
            }
            self.debugger_change.note_refusal(refusal);
        }
        if opened {
            // aterm's panel, with its icon to drag into the list, joins the page
            // when System Settings opened at it.
            if fda_page_up {
                #[expect(
                    clippy::disallowed_methods,
                    reason = "the owner's own press of the Security page's Open Settings"
                )]
                self.fda_page_opened_by_owner(crate::devtools_helper::Origin::Settings);
            }
            self.note_macos_access_settings_opened(std::time::Instant::now());
        }
        if !devtools_by.is_empty() {
            let outcome = self.hand_devtools_to_owner(crate::devtools_helper::Origin::Settings);
            if outcome == crate::devtools_helper::HandOver::Listing {
                self.devtools_helper
                    .wait_in_settings(devtools_by.iter().copied());
            }
            self.note_devtools_outcome_on(&devtools_by, outcome, None);
        }
    }

    /// Transfer the already-open pane's guard before dispatch returns. A
    /// queued close may dispose of this view before the ordinary posture pass.
    /// The notification and every other Settings gesture still drain there.
    pub(crate) fn take_settings_pane_dialog(&mut self, view: crate::tab_model::ViewId) {
        let dialog = match self.native_runtime.view_state_mut(view) {
            Some(crate::native_app::AppViewState::Settings(state)) => {
                state.take_consent_pane_dialog()
            }
            _ => None,
        };
        self.adopt_access_dialog(dialog, std::time::Instant::now());
    }

    /// What the Developer Tools hand-over came to, on the bar of each Settings view
    /// in `views` that pressed for it (`SettingsViewState::note_devtools_hand_over`),
    /// each told to draw it: written outside the reducer.
    pub(crate) fn note_devtools_outcome_on(
        &mut self,
        views: &[(crate::WindowId, crate::tab_model::ViewId)],
        outcome: crate::devtools_helper::HandOver,
        unlisted: Option<&str>,
    ) {
        for &(wid, view) in views {
            let noted = match self.native_runtime.view_state_mut(view) {
                Some(crate::native_app::AppViewState::Settings(state)) => {
                    state.note_devtools_hand_over(outcome, unlisted)
                }
                _ => false,
            };
            if noted && let Some(window) = self.windows.get_mut(&wid) {
                window.last_present = None;
                if let Some(os_window) = window.os_window.as_ref() {
                    os_window.request_redraw();
                }
            }
        }
    }

    /// Consume the first confirmed request in the stable view traversal order. Other
    /// queued requests are drained as superseded, never reported as writes that ran.
    #[cfg(any(target_os = "macos", test))]
    fn apply_askpass_off_requests(
        &mut self,
        requests: Vec<(
            crate::WindowId,
            crate::tab_model::ViewId,
            bool,
            crate::native_settings::OwnerYes,
        )>,
        persist: impl FnOnce(bool, crate::native_settings::OwnerYes) -> std::io::Result<()>,
    ) {
        let mut requests = requests.into_iter();
        let Some((wid, view, off, owner)) = requests.next() else {
            return;
        };
        let written = persist(off, owner).map_err(|error| error.to_string());
        let first = std::iter::once((
            wid,
            view,
            written.as_ref().map(|()| off).map_err(String::as_str),
        ));
        let rest = requests.map(|(wid, view, _off, _owner)| {
            (wid, view, Err("another confirmed preference change was already queued; this request did not run"))
        });
        for (wid, view, result) in first.chain(rest) {
            let noted = match self.native_runtime.view_state_mut(view) {
                Some(crate::native_app::AppViewState::Settings(state)) => {
                    state.note_askpass_off_outcome(result)
                }
                _ => false,
            };
            if noted && let Some(window) = self.windows.get_mut(&wid) {
                window.last_present = None;
                if let Some(os_window) = window.os_window.as_ref() {
                    os_window.request_redraw();
                }
            }
        }
    }

    fn macos_access_projection(&mut self, on_screen: bool) -> crate::native_settings::MacosAccess {
        let facts = if on_screen {
            self.consent_panel_facts()
        } else {
            self.consent_panel_facts_cached()
        };
        let warmup_offered =
            self.config.privacy_enabled() && self.config.privacy_warmup().offered();
        let tccutil = tccutil_presence_once();
        let warmup_live = self.consent_warmup_live();
        let warmup_rows = self.consent_warmup_rows().to_vec();
        let evidence = aterm_containment::SpikeEvidence::MEASURED;
        let split = crate::privacy_report::covers_split(facts.fda, evidence);
        let (claimants, _) = self
            .consent
            .claimants(facts.bundle_id.as_deref(), facts.enabled);
        crate::native_settings::MacosAccess {
            enabled: facts.enabled,
            anchor: aterm_containment::image_anchor(),
            fda: facts.fda,
            probe: facts.probe,
            dr: facts.dr,
            evidence,
            bundle_id: facts.reset_subject,
            install: facts.install,
            running: facts.running,
            sessions_total: facts.sessions_total,
            sessions_adopted: facts.sessions_adopted,
            covers: split.covers,
            uncovered: split.uncovered,
            unmeasured: split.unmeasured,
            tccutil,
            warmup_offered,
            warmup_live,
            warmup_rows,
            reset: None,
            claimants: claimants.map(|(census, _age)| census),
            trash_tool: crate::consent_retire::trash_tool_present(),
            retire_live: self.claimant_retire.is_live(),
            retired: self.claimant_retire.report(),
            retire_status: self.claimant_retire_status(),
            debugger: self.debugger_access_facts(),
            devtools: self.devtools_access_facts(on_screen),
            #[cfg(target_os = "macos")]
            askpass_relay_off: crate::askpass::relay_off(),
            #[cfg(not(target_os = "macos"))]
            askpass_relay_off: false,
        }
    }

    fn devtools_access_facts(&mut self, on_screen: bool) -> crate::native_settings::DevToolsFacts {
        let gate = self.config.privacy_probe_gate();
        let observer_off = self.config.privacy_observer_off();
        let row = if on_screen {
            self.consent.devtools(gate, observer_off)
        } else {
            self.consent.devtools_cached(gate, observer_off)
        };
        let (list, grant) = match self.devtools_helper.reading() {
            Some(reading)
                if row.pending()
                    && reading.list.label == aterm_containment::consent::DevToolListLabel::Read =>
            {
                (reading.list, reading.grant)
            }
            _ => (row.list(), row.grant()),
        };
        let opened_minutes = self.devtools_helper.last_opened_at().and_then(|at| {
            crate::devtools_helper::minutes_since(at, aterm_time::unix_secs().unwrap_or(0))
        });
        crate::native_settings::DevToolsFacts {
            enabled: gate.permits(),
            probe: row.probe(),
            answer_minutes: row.answer_age_ms().map(|ms| ms / 60_000),
            list,
            grant,
            opened_minutes,
            declined: self.devtools_helper.declined_marker(),
        }
    }

    /// What the Security page's debugger row renders.
    pub(crate) fn debugger_access_facts(&mut self) -> crate::debugger_access::DebuggerFacts {
        let row = self.consent.debugger(self.config.privacy_probe_gate());
        self.debugger_access_facts_from(row)
    }

    /// [`Self::debugger_access_facts`] over a row already read.
    pub(crate) fn debugger_access_facts_from(
        &mut self,
        row: crate::consent_state::DebuggerRow,
    ) -> crate::debugger_access::DebuggerFacts {
        let gate = self.config.privacy_probe_gate();
        if !row.pending() {
            let step = self
                .debugger_change
                .observe(row.probe(), crate::debugger_access::now_secs());
            self.apply_debugger_step(step);
        }
        self.debugger_change
            .facts(gate.permits(), row.probe(), row.self_hardened())
    }

    fn claimant_retire_status(&self) -> Option<String> {
        if self.claimant_retire.is_live() {
            return Some("Moving copies of aterm to the Trash\u{2026}".to_string());
        }
        if let Some(why) = self.claimant_retire_refusal {
            return Some(format!("Nothing was moved: {why}."));
        }
        let report = self.claimant_retire.report()?;
        let moved = report
            .iter()
            .filter(|(_, outcome)| *outcome == aterm_containment::consent::Retired::Moved)
            .count();
        Some(crate::consent_retire::summary(moved, report.len()))
    }

    /// Locate the Settings view compatibility control verbs target.
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

    /// Locate whichever Settings view is actually visible in the front window's active tab.
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

    /// Locate a live presentation of one exact Settings route for a compatibility alias.
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

    /// Overlay-model access for the model-level tests.
    #[cfg(test)]
    pub(crate) fn native_settings_legacy_state(&self) -> Option<&crate::settings::SettingsState> {
        self.native_settings_view_target()
            .map(|(_, _, _, state)| &state.legacy)
    }

    /// Whether window `wid` holds a Settings tab ([`Self::visit_settings_tabs_in_window`]).
    pub(crate) fn window_holds_settings(&self, wid: crate::WindowId) -> bool {
        let mut holds = false;
        self.visit_settings_tabs_in_window(wid, |_, _, _, _| holds = true);
        holds
    }

    /// Select the process-singleton Settings app in the frontmost window.
    pub(crate) fn open_settings_tab(
        &mut self,
        route: crate::native_settings::SettingsRoute,
    ) -> bool {
        let Some(wid) = self.frontmost_window else {
            return false;
        };
        self.open_settings_tab_in_window(wid, route)
    }

    /// Hand a new Settings view where aterm.toml lives.
    pub(crate) fn arm_settings_config_file(
        &self,
        state: &mut crate::native_settings::SettingsViewState,
    ) {
        let path = self
            .native_config_service
            .bound_logical_path()
            .map(std::path::Path::to_path_buf)
            .or_else(crate::app_config::config_path);
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        state.arm_config_file(
            path,
            home.as_deref(),
            crate::native_settings::ConfigFileArm::for_instance(self.headless),
        );
    }

    /// Whether window `wid`'s front view is Settings on its Packages page.
    pub(crate) fn packages_page_in_front(&self, wid: crate::WindowId) -> bool {
        self.active_native_view(wid).is_some_and(|(_, view)| {
            matches!(
                self.native_runtime.view_state(view),
                Some(crate::native_app::AppViewState::Settings(state))
                    if state.route == crate::native_settings::SettingsRoute::Packages
            )
        })
    }

    /// Window `wid`'s front tab moved, or the window became key: when its front is the Packages
    /// page, read the package status again, so what a terminal changed meanwhile shows on it (the
    /// LAN sharing switch after `aterm pkg serve-units --off|--on`, an install).
    pub(crate) fn packages_page_met_the_front(&mut self, wid: crate::WindowId) {
        if self.packages_page_in_front(wid) {
            self.start_native_packages_refresh();
        }
    }

    /// [`Self::open_settings_tab`] in a named window.
    pub(crate) fn open_settings_tab_in_window(
        &mut self,
        wid: crate::WindowId,
        route: crate::native_settings::SettingsRoute,
    ) -> bool {
        let existing = self
            .settings_tab_in_window(wid)
            .map(|(index, tab, instance, view)| (wid, index, tab, instance, view));

        let (wid, _index, tab_id, _instance, view) = if let Some(existing) = existing {
            existing
        } else {
            let snapshot = self.native_config_service.snapshot();
            let mut state = crate::native_settings::SettingsViewState::from_snapshot(&snapshot)
                .expect("versioned config service snapshots are valid Settings input");
            self.arm_settings_config_file(&mut state);
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
                let app = crate::native_app::NativeApp::Settings(Box::new(
                    crate::native_settings::SettingsApp::new_at_config_revision(
                        self.update_snapshot(checking),
                        snapshot.revision,
                    ),
                ));
                self.install_new_native_tab(wid, app, view_state, presentation)
            };
            let Ok((instance, tab_id, view)) = install else {
                return false;
            };
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
            let focused = ws
                .tab_set
                .active_mut()
                .is_some_and(|tab| tab.set_focus(view));
            if !focused {
                return false;
            }
            crate::app_tabs::cancel_pending_close(ws);
            ws.last_present = None;
        }
        self.resync_active_or_window(wid);
        // A window holding Settings is never smaller than Settings' least.
        self.hold_least_window_size(wid);
        self.sync_settings_consent_posture();
        self.sync_settings_session_runtime();
        if route == crate::native_settings::SettingsRoute::SoftwareUpdate {
            self.acknowledge_native_update_attention();
        }
        self.request_native_update_reconcile(
            crate::app_native::NativeUpdateReconcilePurpose::Refresh,
        );
        self.note_native_package_tool();
        self.publish_native_packages_state();
        self.start_native_packages_refresh();
        self.publish_native_messages_state();
        self.publish_native_harness_state();
        self.start_native_machine_refresh();
        let action_id = crate::native_settings::route_action(route);
        let action = crate::native_app::AppEvent::Action(crate::native_app::ActionInvocation {
            id: action_id,
            value: None,
        });
        if self.dispatch_native_view_event(wid, view, action).is_err() {
            return false;
        }
        self.sync_settings_consent_posture();
        self.sync_settings_session_runtime();
        self.sync_active_session();
        if let Some(window) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
            window.focus_window();
            window.request_redraw();
        }
        true
    }

    /// Hand the Settings view in `wid` (the one [`Self::open_settings_tab_in_window`] raised).
    pub(crate) fn dispatch_settings_action_in_window(
        &mut self,
        wid: crate::WindowId,
        id: &str,
        value: Option<crate::native_app::SemanticInput>,
    ) -> bool {
        let Some((_, _, _, view)) = self.settings_tab_in_window(wid) else {
            return false;
        };
        let action = crate::native_app::AppEvent::Action(crate::native_app::ActionInvocation {
            id: crate::native_ui::ActionId::new(id),
            value,
        });
        self.dispatch_native_view_event(wid, view, action).is_ok()
    }

    /// Close every presentation of the native Settings singleton.
    pub(crate) fn close_settings_tabs(&mut self) -> bool {
        let mut targets = Vec::new();
        for &wid in self.windows.keys() {
            self.visit_settings_tabs_in_window(wid, |_, tab, _, view| {
                targets.push((wid, tab, view));
            });
        }
        let mut removed = false;
        for (wid, tab, view) in targets {
            // A Settings tab behind the person's front tab closes without taking the front.
            removed |= self.close_view_keeping_front(wid, tab, view).is_ok();
        }
        removed
    }

    /// Apply the compatibility `settings [open|close]` request.
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

    /// `settings section <name>` (socket).
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

    /// The voice to audition after a config swap.
    pub(crate) fn typing_sound_to_audition_on_swap(
        &self,
        next: &crate::app_config::Config,
    ) -> Option<aterm_effects::trail_sound::SoundVoice> {
        let next_voice = next.trail_sound_voice();
        (next_voice != self.typing_sound_auditioned).then_some(next_voice)
    }

    /// Play one keystroke of `voice` so a voice is chosen by ear.
    pub(crate) fn audition_typing_sound(&mut self, voice: aterm_effects::trail_sound::SoundVoice) {
        use aterm_effects::trail_sound::{SoundEvent, SoundGesture, SoundKind};
        self.typing_sound_auditioned = voice;
        let volume = self.config.trail_sound_volume();
        let audible = crate::app_input::keystroke_click_audible(
            self.trail_audio.host_state(),
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
            shifted: false,
        });
    }

    /// Push the front window's open overlay accessibility tree.
    pub(crate) fn overlay_a11y_update(&mut self) {
        #[cfg(a11y_tree)]
        if let Some(wid) = self.frontmost_window {
            self.push_a11y_tree(wid);
        }
    }

    /// Build the accessibility tree for window `wid`'s open overlay.
    #[cfg(a11y_tree)]
    pub(crate) fn push_a11y_tree(&mut self, wid: crate::WindowId) {
        self.push_a11y_tree_with_plan(wid, None);
    }

    #[cfg(a11y_tree)]
    pub(crate) fn push_a11y_tree_with_plan(
        &mut self,
        wid: crate::WindowId,
        frame_plan: Option<&crate::tab_model::VisibleLeafPlan>,
    ) {
        if self.windows.get(&wid).is_none_or(|ws| !ws.a11y_active) {
            return;
        }
        let has_overlay = self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.overlay.is_some());
        let owned_plan = (!has_overlay && frame_plan.is_none())
            .then(|| self.active_visible_leaf_plan(wid))
            .flatten();
        let plan = frame_plan.or(owned_plan.as_ref());
        // Read before a page's tree reads them again: output landing between the two is then told
        // again, never lost. An overlay's tree holds no terminal, so it says nothing of what the
        // client has read of one.
        let reads = (!has_overlay).then(|| self.client_reads(wid, plan));
        let native_update = (!has_overlay)
            .then(|| match plan {
                Some(plan) => self.take_native_accessibility_update_from_plan(wid, plan),
                None => self.take_native_accessibility_update(wid),
            })
            .flatten();
        let grid_update = reads
            .as_ref()
            .filter(|_| native_update.is_none())
            .and_then(|reads| self.grid_a11y_tree_reading(wid, reads));
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let (update, published) = match &ws.overlay {
            Some(o) => (crate::overlay::OverlayModel::a11y(o.model()), None),
            None => match native_update {
                Some(Ok((update, published))) => (update, Some(published)),
                Some(Err(_)) => (crate::a11y_tree::empty_tree(), None),
                None => (
                    grid_update.unwrap_or_else(crate::a11y_tree::empty_tree),
                    None,
                ),
            },
        };
        ws.native_a11y_published = published;
        if ws.native_a11y_published.is_none() {
            ws.native_a11y_staged = None;
        }
        if let Some(reads) = reads {
            ws.a11y_reads = reads;
        }
        ws.a11y_parked_held = false;
        #[cfg(test)]
        ws.a11y_sent.push(update.clone());
        let Some(adapter) = ws.a11y.as_mut() else {
            return;
        };
        adapter.update_if_active(move || update);
    }

    /// The terminal grid's tree for window `wid` (`a11y_tree::grid_tree`), read as a check reads it.
    #[cfg(all(a11y_tree, any(test, debug_assertions)))]
    pub(crate) fn grid_a11y_tree(
        &self,
        wid: crate::WindowId,
        frame_plan: Option<&crate::tab_model::VisibleLeafPlan>,
    ) -> Option<aterm_a11y::model::TreeUpdate> {
        let initial_plan = frame_plan
            .is_none()
            .then(|| self.active_visible_leaf_plan(wid))
            .flatten();
        let reads = self.client_reads(wid, frame_plan.or(initial_plan.as_ref()));
        self.grid_a11y_tree_reading(wid, &reads)
    }

    /// What a client must hear of window `wid` now, laid out by `plan`.
    #[cfg(a11y_tree)]
    pub(crate) fn client_reads(
        &self,
        wid: crate::WindowId,
        plan: Option<&crate::tab_model::VisibleLeafPlan>,
    ) -> crate::a11y_tree::ClientReads {
        let Some(window) = self.windows.get(&wid) else {
            return crate::a11y_tree::ClientReads::default();
        };
        let mut frames = window.a11y_document_frames.borrow_mut();
        let mut pass = frames.pass();
        let panes = plan
            .map(|plan| self.grid_a11y_panes_in_pass(wid, plan, &mut pass))
            .unwrap_or_default();
        let front = panes
            .is_empty()
            .then_some((usize::from(window.rows), usize::from(window.cols)))
            .zip(self.front_terminal(wid).map(|front| front.session))
            .and_then(|((rows, cols), session)| {
                self.terminal_document_in_pass(wid, session, rows, cols, true, &mut pass)
            });
        crate::a11y_tree::ClientReads {
            views: plan.map_or_else(Vec::new, |plan| {
                plan.leaves.iter().map(|leaf| leaf.view).collect()
            }),
            panes,
            front,
            rewind: self.rewind_a11y(wid, plan),
            questions: self.grid_a11y_questions(wid),
            menu: self.grid_a11y_menu(wid),
            rename: self.grid_a11y_rename(wid),
            find: self.grid_a11y_find(wid),
        }
    }

    /// The terminal grid's tree for window `wid`, its terminals as `reads` holds them.
    #[cfg(a11y_tree)]
    fn grid_a11y_tree_reading(
        &self,
        wid: crate::WindowId,
        reads: &crate::a11y_tree::ClientReads,
    ) -> Option<aterm_a11y::model::TreeUpdate> {
        let (rows, cols) = {
            let ws = self.windows.get(&wid)?;
            (usize::from(ws.rows), usize::from(ws.cols))
        };
        let panes = &reads.panes;
        let empty = crate::accessibility::AccessibleSnapshot {
            text: String::new(),
            rows: if panes.is_empty() { 0 } else { rows },
            cols,
            cursor: None,
        };
        let (snap, selection) = reads.front.as_ref().map_or((&empty, None), |document| {
            (&document.snap, document.selection)
        });
        let geometry = self.grid_a11y_geometry(wid);
        let tabs = self.grid_a11y_tabs(wid);
        let messages = self.grid_a11y_messages(wid);
        let chips = self.claude_chips_a11y(wid);
        let stats = self.stats_a11y(wid);
        let window_title = self
            .windows
            .get(&wid)
            .map(|ws| ws.current_title.as_str())
            .unwrap_or_default();
        Some(crate::a11y_tree::grid_tree(
            snap,
            geometry,
            &crate::a11y_tree::GridFrame {
                tabs: &tabs,
                panes,
                selection,
                find: reads.find.as_ref(),
                rename: reads.rename.as_ref(),
                messages: &messages,
                chips: &chips,
                menu: reads.menu.as_ref(),
                stats: stats.as_ref(),
                rewind: reads.rewind.as_ref(),
                window_title,
            },
        ))
    }

    /// Where window `wid`'s visible grid (and the tab strip above it) sits in the client
    /// area, in physical pixels (the space the AT-SPI adapter reports root bounds in),
    /// from the window's own metrics.
    #[cfg(a11y_tree)]
    pub(crate) fn grid_a11y_geometry(
        &self,
        wid: crate::WindowId,
    ) -> Option<crate::a11y_tree::GridGeometry> {
        // An unknown window would fall back to the shared backend metrics, another
        // window's geometry; fail closed instead.
        if !self.windows.contains_key(&wid) {
            return None;
        }
        let (cw, ch) = self.win_cell_size(wid);
        if cw == 0 || ch == 0 {
            return None;
        }
        let strip_rows = usize::from(self.tab_strip_rows);
        let strip_y = self.win_pad_top(wid).saturating_add(self.win_head(wid));
        // The frame sits centred in the surface (`frame_origin`), as native leaves are placed.
        let (frame_x, frame_y) = self.frame_origin(wid);
        let (frame_x, frame_y) = (frame_x as f64, frame_y as f64);
        Some(crate::a11y_tree::GridGeometry {
            origin_x: frame_x + self.win_pad(wid) as f64,
            origin_y: frame_y + self.native_content_origin_y(wid) as f64,
            cell_w: cw as f64,
            cell_h: ch as f64,
            strip_y: frame_y + strip_y as f64,
            strip_h: (strip_rows * ch) as f64,
        })
    }

    /// The in-grid tab strip of window `wid` as publishable a11y items, or empty.
    #[cfg(a11y_tree)]
    fn grid_a11y_tabs(&self, wid: crate::WindowId) -> Vec<crate::a11y_tree::GridTab> {
        if self.tab_strip_rows == 0 {
            return Vec::new();
        }
        let Some(ws) = self.windows.get(&wid) else {
            return Vec::new();
        };
        if ws.tab_set.shown_len() < 2 {
            return Vec::new();
        }
        let active = ws.tab_set.strip_active();
        ws.tab_segments
            .iter()
            .filter_map(|segment| {
                let crate::tab_bar::TabHit::Select(index) = segment.kind else {
                    return None;
                };
                if segment.solo {
                    return None;
                }
                let tab = ws.tab_set.tab_at(index)?;
                Some(crate::a11y_tree::GridTab {
                    index,
                    id: tab.id.get(),
                    title: ws.strip_titles_scratch.get(index).map_or_else(
                        || "aterm".to_string(),
                        |label| crate::chrome_label::capped_label(label).into_owned(),
                    ),
                    selected: active == Some(index),
                    start_col: segment.start_col,
                    end_col: segment.end_col,
                    // By the tab's own focused leaf: `layouts` counts only all-terminal tabs.
                    theme: self
                        .view_store
                        .get(tab.focus)
                        .copied()
                        .and_then(crate::tab_model::View::terminal_session)
                        .and_then(|session| self.session_choice(session))
                        .map(|choice| choice.selector),
                })
            })
            .collect()
    }

    /// The terminals of a split or mixed tab `plan` as one document each, read from each pane's own
    /// terminal.
    #[cfg(a11y_tree)]
    fn grid_a11y_panes(
        &self,
        wid: crate::WindowId,
        plan: &crate::tab_model::VisibleLeafPlan,
    ) -> Vec<crate::a11y_tree::GridPane> {
        let Some(window) = self.windows.get(&wid) else {
            return Vec::new();
        };
        let mut frames = window.a11y_document_frames.borrow_mut();
        let mut pass = frames.pass();
        self.grid_a11y_panes_in_pass(wid, plan, &mut pass)
    }

    #[cfg(a11y_tree)]
    fn grid_a11y_panes_in_pass(
        &self,
        wid: crate::WindowId,
        plan: &crate::tab_model::VisibleLeafPlan,
        pass: &mut crate::accessibility::DocumentFramePass<'_>,
    ) -> Vec<crate::a11y_tree::GridPane> {
        if plan.leaves.len() < 2 || plan.zoomed {
            return Vec::new();
        }
        plan.leaves
            .iter()
            .enumerate()
            .filter_map(|(slot, leaf)| self.grid_a11y_pane(wid, plan, slot, leaf, pass))
            .collect()
    }

    /// Window `wid`'s mixed-tab tree `update` with the terminal leaves of `plan` beside its
    /// native leaves, each one terminal document read from its terminal.
    #[cfg(a11y_tree)]
    pub(crate) fn with_mixed_terminal_panes(
        &self,
        wid: crate::WindowId,
        plan: &crate::tab_model::VisibleLeafPlan,
        mut update: aterm_a11y::model::TreeUpdate,
    ) -> Result<aterm_a11y::model::TreeUpdate, String> {
        let panes = self.grid_a11y_panes(wid, plan);
        if panes.is_empty() {
            return Ok(update);
        }
        crate::a11y_tree::splice_terminal_panes(
            &mut update,
            &panes,
            self.grid_a11y_geometry(wid),
            plan.leaves.len(),
        )
        .map_err(|id| format!("native accessibility terminal pane id {id:?} collides"))?;
        Ok(update)
    }

    /// Window `wid`'s native-tab tree `update` with the window chrome the frame painted around
    /// its leaves: the tab list, stats, messages, find field, Rewind view, harness chips and menu.
    #[cfg(a11y_tree)]
    pub(crate) fn with_window_chrome(
        &self,
        wid: crate::WindowId,
        plan: &crate::tab_model::VisibleLeafPlan,
        mut update: aterm_a11y::model::TreeUpdate,
    ) -> Result<aterm_a11y::model::TreeUpdate, String> {
        let Some(cols) = self.windows.get(&wid).map(|ws| usize::from(ws.cols)) else {
            return Ok(update);
        };
        let tabs = self.grid_a11y_tabs(wid);
        let find = self.grid_a11y_find(wid);
        let rename = self.grid_a11y_rename(wid);
        let messages = self.grid_a11y_messages(wid);
        let chips = self.claude_chips_a11y(wid);
        let menu = self.grid_a11y_menu(wid);
        let stats = self.stats_a11y(wid);
        let rewind = self.rewind_a11y(wid, Some(plan));
        crate::a11y_tree::splice_chrome(
            &mut update,
            self.grid_a11y_geometry(wid),
            &crate::a11y_tree::GridFrame {
                tabs: &tabs,
                find: find.as_ref(),
                rename: rename.as_ref(),
                messages: &messages,
                chips: &chips,
                menu: menu.as_ref(),
                stats: stats.as_ref(),
                rewind: rewind.as_ref(),
                ..crate::a11y_tree::GridFrame::default()
            },
            cols,
        )
        .map_err(|id| format!("native accessibility chrome id {id:?} collides"))?;
        Ok(update)
    }

    /// The terminal document of `plan`'s leaf `leaf` at `slot`, or `None` for a non-terminal leaf.
    #[cfg(a11y_tree)]
    fn grid_a11y_pane(
        &self,
        wid: crate::WindowId,
        plan: &crate::tab_model::VisibleLeafPlan,
        slot: usize,
        leaf: &crate::tab_model::VisibleLeaf,
        pass: &mut crate::accessibility::DocumentFramePass<'_>,
    ) -> Option<crate::a11y_tree::GridPane> {
        let session = self
            .view_store
            .get(leaf.view)
            .copied()
            .and_then(crate::tab_model::View::terminal_session)?;
        let row_off = leaf.rect.origin.y.round().max(0.0) as usize;
        let col_off = leaf.rect.origin.x.round().max(0.0) as usize;
        let rows = (leaf.rect.size.height.round() as usize).max(1);
        let cols = (leaf.rect.size.width.round() as usize).max(1);
        let document =
            self.terminal_document_in_pass(wid, session, rows, cols, leaf.focused, pass)?;
        let mut label = format!(
            "{} — pane {} of {}",
            crate::accessibility::LABEL,
            slot + 1,
            plan.leaves.len()
        );
        // The slider says this of the focused pane's past; any other's has only its label to say it.
        if self.rewound_front(wid) != Some(session)
            && let Some(words) = self.rewind_words(wid, session)
        {
            label.push_str(", ");
            label.push_str(&words);
        }
        Some(crate::a11y_tree::GridPane {
            slot,
            label,
            snap: document.snap,
            row_off,
            col_off,
            focused: leaf.focused,
            selection: document.selection,
        })
    }

    /// Read one terminal document through a complete window frame pass for a test.
    #[cfg(all(test, a11y_tree))]
    pub(crate) fn terminal_document_for_test(
        &self,
        wid: crate::WindowId,
        session: u64,
        rows: usize,
        cols: usize,
        focused: bool,
    ) -> Option<crate::a11y_tree::TerminalDocument> {
        let window = self.windows.get(&wid)?;
        let mut frames = window.a11y_document_frames.borrow_mut();
        let mut pass = frames.pass();
        self.terminal_document_in_pass(wid, session, rows, cols, focused, &mut pass)
    }

    /// What session `session` shows a client in a view of `rows` by `cols` in window `wid`, read
    /// from its terminal: the rows a frame of that view draws (the past screen while rewound), and
    /// when `focused` the caret and any composition in progress there. The window's composed cells
    /// cannot answer this, since they hold another tab's or layout's rows until a frame of this
    /// one, a frame that proves nothing changed leaves them without their chrome rows, and they
    /// carry what aterm paints over a program, which reaches a client as its own node.
    /// The pass owns refillable storage; extraction does not consume paint damage.
    #[cfg(a11y_tree)]
    fn terminal_document_in_pass(
        &self,
        wid: crate::WindowId,
        session: u64,
        rows: usize,
        cols: usize,
        focused: bool,
        pass: &mut crate::accessibility::DocumentFramePass<'_>,
    ) -> Option<crate::a11y_tree::TerminalDocument> {
        let (term, owner) = self.pool.shown(session)?;
        let cells = pass.frame(rows, cols);
        let ambiguous_cjk = {
            let site = crate::metrics::TermWaitSite::Redraw;
            let mut term = crate::term_lock_ui(&term, &owner.ctx.ui_waiting, site);
            term.text_frame_into(cells, rows, cols);
            term.modes().ambiguous_width_double
        };
        let composing = self
            .windows
            .get(&wid)
            .filter(|ws| focused && !ws.preedit.is_empty() && cells.display_offset() == 0)
            .filter(|_| {
                matches!(
                    self.preedit_owner(wid),
                    crate::app_input::PreeditOwner::Grid
                )
            });
        if let Some(ws) = composing {
            cells.overlay_ime_preedit(&ws.preedit, ws.preedit_caret, ambiguous_cjk);
        }
        let cursor = cells.cursor().filter(|&(_, col)| focused && col < cols);
        let snap = crate::accessibility::AccessibleSnapshot::from_cells_in(
            cells.cells(),
            0..rows,
            0..cols,
            cursor,
        );
        let selection = crate::a11y_tree::selected_span(cells, &snap);
        Some(crate::a11y_tree::TerminalDocument { snap, selection })
    }

    /// The find panel's query field for window `wid`, or `None` when find mode is off.
    #[cfg(a11y_tree)]
    pub(crate) fn grid_a11y_find(
        &self,
        wid: crate::WindowId,
    ) -> Option<crate::a11y_tree::GridFind> {
        let search = self.windows.get(&wid)?.search.as_ref()?;
        let (query, caret) = self.field_as_composed(
            wid,
            crate::app_input::PreeditOwner::Find,
            &search.query,
            search.cursor,
        );
        let mut status: Vec<String> = search.spoken_status().into_iter().collect();
        if search.case_sensitive {
            status.push("case sensitive".to_string());
        }
        if search.is_regex {
            status.push("regular expression".to_string());
        }
        Some(crate::a11y_tree::GridFind {
            query,
            caret,
            status: status.join("; "),
        })
    }

    /// The field open over window `wid`'s tab strip, or `None` when it has none.
    #[cfg(a11y_tree)]
    fn grid_a11y_rename(&self, wid: crate::WindowId) -> Option<crate::a11y_tree::GridRename> {
        let edit = self.inline_rename_edit(wid)?;
        let (text, caret) = self.field_as_composed(
            wid,
            crate::app_input::PreeditOwner::Rename,
            &edit.text,
            edit.cursor,
        );
        let label = match edit.field {
            crate::session_timeline::MetaField::Role => "Session role",
            _ => "Session name",
        };
        Some(crate::a11y_tree::GridRename { label, text, caret })
    }

    /// `text` with the composition in progress when `owner` holds it, and the caret as a character
    /// index for a field whose caret is the byte offset `cursor`.
    #[cfg(a11y_tree)]
    fn field_as_composed(
        &self,
        wid: crate::WindowId,
        owner: crate::app_input::PreeditOwner,
        text: &str,
        cursor: usize,
    ) -> (String, usize) {
        let (head, tail) = text.split_at_checked(cursor).unwrap_or((text, ""));
        let window = self
            .windows
            .get(&wid)
            .filter(|_| self.preedit_owner(wid) == owner);
        let composing = window.map_or("", |ws| ws.preedit.as_str());
        let composed = window
            .and_then(|ws| ws.preedit_caret)
            .and_then(|at| composing.get(..at))
            .unwrap_or(composing);
        (
            format!("{head}{composing}{tail}"),
            head.chars().count() + composed.chars().count(),
        )
    }

    /// The chrome messages window `wid` is showing now: surfaces that leave again, which all but a
    /// hovered link's caption say unprompted, since a screen reader cannot find them by exploring.
    /// The caption stays silent in the tree until a client reads it.
    #[cfg(a11y_tree)]
    fn grid_a11y_messages(&self, wid: crate::WindowId) -> Vec<crate::a11y_tree::GridMessage> {
        use crate::a11y_tree::{ChromeMessage, GridMessage};
        let Some(ws) = self.windows.get(&wid) else {
            return Vec::new();
        };
        let mut out = self.grid_a11y_questions(wid);
        if let Some(caption) = ws.shown_caption.as_ref().filter(|caption| {
            self.front_terminal(wid)
                .is_some_and(|front| front.session == caption.session)
        }) {
            out.push(GridMessage {
                message: ChromeMessage::LinkCaption,
                text: caption.words.clone(),
                detail: None,
            });
        }
        out
    }

    /// The question waiting on an answer in window `wid`. A paste question hides a close question
    /// until it is answered, as it does on the band and for the keys.
    #[cfg(a11y_tree)]
    fn grid_a11y_questions(&self, wid: crate::WindowId) -> Vec<crate::a11y_tree::GridMessage> {
        use crate::a11y_tree::{ChromeMessage, GridMessage};
        let mut out: Vec<GridMessage> = Vec::new();
        if let Some(pending) = self.paste_banner.as_ref().filter(|p| p.wid == wid) {
            out.push(GridMessage {
                message: ChromeMessage::PasteConfirm,
                text: crate::paste_banner::question(pending.text()),
                detail: Some(crate::paste_banner::announcement_detail(pending.text())),
            });
        } else if let Some(pending) = self.close_band(wid) {
            out.push(GridMessage {
                message: ChromeMessage::CloseConfirm,
                text: crate::close_confirm::question(&pending.prompt),
                detail: Some(crate::close_confirm::announcement_detail(&pending.prompt)),
            });
        }
        out
    }

    /// The popup menu open over window `wid`'s grid: a tab's, which paints over Autopilot's and takes
    /// the keys first, else Autopilot's.
    #[cfg(a11y_tree)]
    fn grid_a11y_menu(&self, wid: crate::WindowId) -> Option<crate::a11y_tree::GridMenu> {
        self.tab_grid_menu(wid)
            .or_else(|| self.autopilot_grid_menu(wid))
    }

    /// Handle an event from a window's native accessibility adapter.
    #[cfg(a11y_tree)]
    pub(crate) fn on_accessibility_event(&mut self, event: crate::a11y_adapter::Event) {
        use crate::a11y_adapter::WindowEvent as AkWindowEvent;
        let Some(wid) = self.winit_to_window.get(&event.window_id).copied() else {
            return;
        };
        match event.window_event {
            AkWindowEvent::InitialTreeRequested => {
                crate::a11y_backend::note_at_client();
                if let Some(ws) = self.windows.get_mut(&wid) {
                    ws.a11y_active = true;
                    #[cfg(target_os = "linux")]
                    if let (Some(adapter), Some(window)) =
                        (ws.a11y.as_mut(), ws.os_window.as_deref())
                    {
                        adapter.listened(window);
                    }
                }
                self.push_a11y_tree(wid);
            }
            AkWindowEvent::ActionRequested(req) => self.on_accessibility_action(wid, req),
            AkWindowEvent::AccessibilityDeactivated => {
                if let Some(ws) = self.windows.get_mut(&wid) {
                    ws.a11y_active = false;
                    ws.a11y_document_frames.get_mut().clear();
                }
            }
        }
    }

    /// Route an OS accessibility `ActionRequest` on `wid`, the whole dispatch marked as an
    /// accessibility client's ([`App::with_assistive_source`]): no person is known to stand
    /// behind it, since every tab's program reaches the window's tree with aterm's own
    /// Accessibility grant, as a screen reader does.
    #[cfg(a11y_tree)]
    pub(crate) fn on_accessibility_action(
        &mut self,
        wid: crate::WindowId,
        req: aterm_a11y::model::ActionRequest,
    ) {
        self.with_assistive_source(|app| app.dispatch_accessibility_request(wid, req));
    }

    /// [`App::on_accessibility_action`]'s routing, under its mark.
    #[cfg(a11y_tree)]
    fn dispatch_accessibility_request(
        &mut self,
        wid: crate::WindowId,
        req: aterm_a11y::model::ActionRequest,
    ) {
        use crate::overlay::OverlayKind;
        self.clear_move_license_from(wid, crate::cursor_glow::BoundaryOrigin::Accessibility);
        let kind = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.overlay.as_ref())
            .map(crate::overlay::Overlay::kind);
        let Some(kind) = kind else {
            // No overlay: the tree is a native tab app's projection or the terminal
            // grid. Only the grid mints tab ids, in a range no other builder uses, so
            // decoding one cannot steal a native app's request. Focus and Click both
            // mean "show me this tab".
            if let Some(index) = crate::a11y_tree::tab_action_index(&req, |index| {
                let ws = self.windows.get(&wid)?;
                if self.tab_strip_rows == 0 || ws.tab_set.shown_len() < 2 {
                    return None;
                }
                ws.tab_set.shown().get(index).map(|tab| tab.id.get())
            }) {
                self.switch_tab_in(wid, index);
                if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
                    w.request_redraw();
                }
                self.push_a11y_tree(wid);
                return;
            }
            // A slider row of an open fact card (its effort slider) steps one stop.
            if let aterm_a11y::model::Action::Increment | aterm_a11y::model::Action::Decrement =
                req.action
                && let Some((epoch, item)) = crate::a11y_tree::menu_item_for(req.target_node)
            {
                let up = req.action == aterm_a11y::model::Action::Increment;
                self.step_autopilot_menu_slider_for_reader(wid, epoch, item, up);
                return;
            }
            if req.action == aterm_a11y::model::Action::Click
                && let Some((epoch, item)) = crate::a11y_tree::menu_item_for(req.target_node)
            {
                if self.tab_grid_menu(wid).is_some() {
                    self.choose_tab_menu_item_for_reader(wid, epoch, item);
                } else {
                    self.choose_autopilot_menu_item_for_reader(wid, epoch, item);
                }
                return;
            }
            if req.action == aterm_a11y::model::Action::ShowContextMenu
                && let Some((session, chip)) = crate::a11y_tree::chip_for(req.target_node)
                && let Some(fact) = chip.fact()
            {
                self.open_fact_menu_for_reader(wid, session, fact);
                return;
            }
            if req.action == aterm_a11y::model::Action::Click
                && let Some((session, chip)) = crate::a11y_tree::chip_for(req.target_node)
            {
                self.press_claude_chip_for_reader(wid, session, chip);
                return;
            }
            if req.target_node == crate::a11y_tree::REWIND
                && let Some(session) = self.rewound_front(wid)
            {
                let delta = match req.action {
                    aterm_a11y::model::Action::Increment => 1,
                    aterm_a11y::model::Action::Decrement => -1,
                    _ => return,
                };
                let _ = self.rewind_step(session, crate::rewind::StepBy::Slice(delta), None);
                return;
            }
            self.on_native_accessibility_action(wid, req);
            return;
        };
        match kind {
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
                    aterm_a11y::model::Action::Focus => {
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
                    aterm_a11y::model::Action::Click => {
                        if let Some(p) = self.windows.get_mut(&wid).and_then(|ws| ws.palette_mut())
                        {
                            p.select(idx);
                        }
                        self.palette_activate_in(wid);
                    }
                    _ => {}
                }
            }
            OverlayKind::Overview => {
                let Some(idx) = self
                    .windows
                    .get(&wid)
                    .and_then(|ws| ws.overview())
                    .and_then(|map| map.a11y_item_index(req.target_node))
                else {
                    return; // root / list container carries no row action
                };
                match req.action {
                    aterm_a11y::model::Action::Focus => {
                        if let Some(m) = self.windows.get_mut(&wid).and_then(|ws| ws.overview_mut())
                        {
                            m.select(idx);
                        }
                        self.overlay_a11y_update();
                        if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref())
                        {
                            w.request_redraw();
                        }
                    }
                    aterm_a11y::model::Action::Click => {
                        if let Some(m) = self.windows.get_mut(&wid).and_then(|ws| ws.overview_mut())
                        {
                            m.select(idx);
                        }
                        self.overview_activate(wid);
                    }
                    _ => {}
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "app_settings_tests.rs"]
mod tests;
