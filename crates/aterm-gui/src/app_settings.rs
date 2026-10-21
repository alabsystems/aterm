// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native Settings-tab host glue: singleton view discovery, open/focus/close, route
//! navigation, and compatibility control projections. Settings is a native tab, never
//! a window overlay.

use crate::App;

/// `/usr/bin/tccutil`'s presence, read once per process (the Security page republishes
/// every park). `TccutilPresence::Unknown` fails closed in `reset_offer`, so a
/// destructive button is never offered on an unproven tool.
fn tccutil_presence_once() -> aterm_containment::TccutilPresence {
    static PRESENCE: std::sync::OnceLock<aterm_containment::TccutilPresence> =
        std::sync::OnceLock::new();
    *PRESENCE.get_or_init(aterm_containment::consent::tccutil_presence)
}

/// The font rows the terminal's latest font generation could not find, for
/// a Settings view opened after that generation.
static SETTINGS_FONT_REFUSALS: std::sync::Mutex<Vec<crate::native_font_catalog::FontRefusal>> =
    std::sync::Mutex::new(Vec::new());

/// The installed fonts the Settings font pickers offer: the Settings
/// controller they were last listed for, and the last list the listing
/// thread sent. A new controller (Settings opened again) lists again, so a
/// font installed in the meantime appears; the list is remembered by
/// aterm-render, so an unchanged font tree costs a few `stat`s.
static SETTINGS_FONT_FAMILIES: std::sync::Mutex<(
    Option<crate::native_app::AppInstanceId>,
    Option<std::sync::Arc<aterm_render::font_catalog::MonospaceFamilies>>,
)> = std::sync::Mutex::new((None, None));

impl App {
    /// Every live Settings presentation in `wid`, focused or not: a split or restore may
    /// put Settings beside a terminal or another app, so focus is not identity.
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
    /// The event-loop park uses this across every window, including windows
    /// containing only terminal tabs, so temporary leaf lists buy no work.
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

    /// Find the Settings presentation in `wid`: active and focused first, then stable
    /// tab/view order.
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
        // The first leaf with the smallest key, in visit order, without collecting.
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

    /// Resolve one Settings view in `wid`, ordered as [`Self::settings_tab_in_window`].
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

    /// THE PASS BEFORE EVERY WAIT that hands each Settings view what it shows
    /// from the App: Security's access posture, the installed fonts and the
    /// faces the terminal draws, and the session runtime. Each step changes a
    /// view only where what it hands differs, so an idle pass moves no view's
    /// content revision (`native_app::CommonViewState::content_epoch`).
    pub(crate) fn sync_settings_views(&mut self) {
        self.sync_settings_consent_posture();
        self.sync_settings_font_families();
        self.sync_settings_session_runtime();
    }

    /// Copy existing in-memory journal/keeper facts into every Settings view.
    /// Never connect to the keeper or touch a journal from the UI thread.
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
        let mut views = Vec::new();
        for &wid in self.windows.keys() {
            self.visit_settings_tabs_in_window(wid, |_, _, _, view| views.push((wid, view)));
        }
        for (wid, view) in views {
            let changed = self
                .native_runtime
                .change_view_state(view, |state| match state {
                    crate::native_app::AppViewState::Settings(state)
                        if state.session_runtime != Some(runtime) =>
                    {
                        state.session_runtime = Some(runtime);
                        true
                    }
                    _ => false,
                });
            if changed && let Some(window) = self.windows.get_mut(&wid) {
                window.last_present = None;
                if let Some(window) = &window.os_window {
                    window.request_redraw();
                }
            }
        }
    }

    /// Hand the installed-font list to every Settings view, listing it off the
    /// event loop first for a Settings controller that has none yet.
    pub(crate) fn sync_settings_font_families(&mut self) {
        let Some(instance) = self
            .native_runtime
            .instance_by_kind(crate::native_app::AppKind::Settings)
        else {
            return;
        };
        let (listed_for, families) = SETTINGS_FONT_FAMILIES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if listed_for != Some(instance) {
            SETTINGS_FONT_FAMILIES
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0 = Some(instance);
            if let Some(proxy) = self.proxy.clone() {
                let _ = crate::qos::spawn_lane_thread(
                    "aterm-font-families",
                    crate::qos::Role::Background,
                    move || {
                        // Cosmetic: the picker offers Automatic and Other…
                        // until the list lands.
                        let families = aterm_render::font_catalog::monospace_families();
                        let _ = proxy.send_event(crate::Wake::FontFamiliesListed(families));
                    },
                );
            }
        }
        if let Some(families) = families {
            self.hand_settings_font_families(&families);
        }
        let refusals = SETTINGS_FONT_REFUSALS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        self.hand_settings_font_refusals(&refusals, false);
        self.hand_settings_drawn_faces();
    }

    /// Give every Settings view what the terminal's sealed font generation
    /// draws each style with, so an unset font row names it ("Automatic (SF
    /// Mono Italic)"). Read off the live renderer's resident faces, no I/O;
    /// a view repaints only when the report changed.
    fn hand_settings_drawn_faces(&mut self) {
        let drawn = self.backend.drawn_faces().cloned();
        let mut views = Vec::new();
        for &wid in self.windows.keys() {
            self.visit_settings_tabs_in_window(wid, |_, _, _, view| views.push((wid, view)));
        }
        for (wid, view) in views {
            let changed = self
                .native_runtime
                .change_view_state(view, |state| match state {
                    crate::native_app::AppViewState::Settings(state) => {
                        state.replace_drawn_faces(drawn.clone())
                    }
                    crate::native_app::AppViewState::Markdown(_)
                    | crate::native_app::AppViewState::Editor(_)
                    | crate::native_app::AppViewState::Recovery(_) => false,
                });
            if changed && let Some(window) = self.windows.get_mut(&wid) {
                window.last_present = None;
                if let Some(os_window) = window.os_window.as_ref() {
                    os_window.request_redraw();
                }
            }
        }
    }

    /// A font generation's refused rows ([`crate::native_font_catalog::FontRefusal`]):
    /// every Settings view marks them, and says so where one was just saved.
    pub(crate) fn publish_settings_font_refusals(
        &mut self,
        refusals: Vec<crate::native_font_catalog::FontRefusal>,
    ) {
        SETTINGS_FONT_REFUSALS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone_from(&refusals);
        self.hand_settings_font_refusals(&refusals, true);
    }

    fn hand_settings_font_refusals(
        &mut self,
        refusals: &[crate::native_font_catalog::FontRefusal],
        announce: bool,
    ) {
        let mut views = Vec::new();
        for &wid in self.windows.keys() {
            self.visit_settings_tabs_in_window(wid, |_, _, _, view| views.push((wid, view)));
        }
        for (wid, view) in views {
            let changed = self
                .native_runtime
                .change_view_state(view, |state| match state {
                    crate::native_app::AppViewState::Settings(state) => {
                        state.replace_font_refusals(refusals.to_vec(), announce)
                    }
                    crate::native_app::AppViewState::Markdown(_)
                    | crate::native_app::AppViewState::Editor(_)
                    | crate::native_app::AppViewState::Recovery(_) => false,
                });
            if changed && let Some(window) = self.windows.get_mut(&wid) {
                window.last_present = None;
                if let Some(os_window) = window.os_window.as_ref() {
                    os_window.request_redraw();
                }
            }
        }
    }

    /// The listing thread's answer ([`crate::Wake::FontFamiliesListed`]).
    pub(crate) fn publish_settings_font_families(
        &mut self,
        families: std::sync::Arc<aterm_render::font_catalog::MonospaceFamilies>,
    ) {
        SETTINGS_FONT_FAMILIES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .1 = Some(std::sync::Arc::clone(&families));
        self.hand_settings_font_families(&families);
    }

    /// Give `families` to every Settings view that lacks it, repainting those.
    fn hand_settings_font_families(
        &mut self,
        families: &std::sync::Arc<aterm_render::font_catalog::MonospaceFamilies>,
    ) {
        let mut views = Vec::new();
        for &wid in self.windows.keys() {
            self.visit_settings_tabs_in_window(wid, |_, _, _, view| views.push((wid, view)));
        }
        for (wid, view) in views {
            let changed = self
                .native_runtime
                .change_view_state(view, |state| match state {
                    crate::native_app::AppViewState::Settings(state) => {
                        state.replace_font_families(std::sync::Arc::clone(families))
                    }
                    crate::native_app::AppViewState::Markdown(_)
                    | crate::native_app::AppViewState::Editor(_)
                    | crate::native_app::AppViewState::Recovery(_) => false,
                });
            if changed && let Some(window) = self.windows.get_mut(&wid) {
                window.last_present = None;
                if let Some(os_window) = window.os_window.as_ref() {
                    os_window.request_redraw();
                }
            }
        }
    }

    /// Repaint every window whose Settings specimen was waiting on the font
    /// worker ([`crate::Wake::SemanticFontReady`]), focused or not, so its
    /// status never sticks on "Loading font".
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
    ///
    /// The cheap `instance_by_kind` guard comes first, and the projection is
    /// equality-gated.
    ///
    /// * It never reaches the OS on a headless instance: it returns before the probe,
    ///   and `ConsentGestures::for_instance`'s headless arm cannot contact
    ///   `NSWorkspace` or spawn anything.
    /// * The warm-up gesture is drained before the projection is built, so rows
    ///   published in the same pass already read *asking…*.
    pub(crate) fn sync_settings_consent_posture(&mut self) {
        if self
            .native_runtime
            .instance_by_kind(crate::native_app::AppKind::Settings)
            .is_none()
        {
            return;
        }
        // Every Settings view: a window can hold more than one (a split, a reopened
        // tab), and a request left on an unchosen one would wait.
        let mut views = Vec::new();
        for &wid in self.windows.keys() {
            self.visit_settings_tabs_in_window(wid, |_, _, _, view| views.push((wid, view)));
        }
        // Every view shares the process's record of the user's declines, and every
        // confirmed press is performed now. Both run on every host: a headless view's
        // arms reach no OS, and `begin_*` refuse there.
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
        // The posture source is injected (`security_posture`): the host's probes
        // on a windowed macOS instance; nothing on a headless one or off macOS,
        // where there is no TCC to describe and the block is absent rather than
        // an invented card; or a development build's staged fixture, which runs
        // no probe and draws with the inert gesture arms.
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

    /// The Settings views of `views` on the Security page: only they carry the
    /// block.
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

    /// Take a staged posture's block off every Settings view that carries one,
    /// when a development build takes the staged posture off
    /// ([`crate::security_posture`]).
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

    /// How many Settings views draw the MACOS ACCESS block now.
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

    /// The Security page's half of [`Self::sync_settings_consent_posture`], over
    /// `views` (every Settings view): build the block once and hand it to each view
    /// on the Security route. The Full Disk Access probe and the DEVELOPER TOOLS
    /// card's `log show` may start only while one of those views is on screen
    /// ([`Self::settings_view_on_screen`]); otherwise both are served from their
    /// slots, so a Security page left in a background tab or a hidden window starts
    /// nothing (debugger design §9.1, O12).
    fn publish_security_page(&mut self, views: Vec<(crate::WindowId, crate::tab_model::ViewId)>) {
        // Only a view on the Security page carries the block, and this runs every park:
        // reading the route first keeps an open-but-elsewhere Settings tab at one enum
        // compare per window.
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

    /// Whether `view` is on screen: a leaf of its window's active tab, in a window
    /// the OS does not report occluded (minimized or fully covered).
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
                    .is_some_and(|tab| tab.root.leaves().contains(&view))
        })
    }

    /// Hand the Security block's projection, and the gesture arms, to each of
    /// `targets` (the Settings views on that page), repainting a view whose
    /// block changed.
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
                    crate::native_app::AppViewState::Markdown(_)
                    | crate::native_app::AppViewState::Editor(_)
                    | crate::native_app::AppViewState::Recovery(_) => false,
                });
            if changed && let Some(window) = self.windows.get_mut(&wid) {
                window.last_present = None;
                if let Some(os_window) = window.os_window.as_ref() {
                    os_window.request_redraw();
                }
            }
        }
    }

    /// The gesture drain for [`Self::sync_settings_consent_posture`], over every
    /// Settings view it found.
    fn drain_settings_gesture_requests(
        &mut self,
        views: &[(crate::WindowId, crate::tab_model::ViewId)],
    ) {
        // `WarmupState`, its automatic-apply hold and the retire worker are `App`-owned
        // and a Settings reducer cannot reach `App`, so a press is recorded on the view
        // and performed here, from every Settings view whatever page it now shows.
        // `begin_consent_warmup` and `begin_claimant_retire` re-check every gate, so a
        // stale request cannot start a worker past config.
        let mut requested_by = Vec::new();
        let mut opened = false;
        let mut fda_page_up = false;
        let mut retire = None;
        let mut debugger = None;
        let mut askpass_requests = Vec::new();
        let mut devtools_by = Vec::new();
        for &(wid, view) in views {
            // A pass that takes no request changes nothing the view draws.
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
                opened |= open;
                fda_page_up |= fda;
                took |= open || fda;
                let retiring = state.take_claimant_retire_request();
                took |= retiring.is_some();
                retire = retire.take().or(retiring);
                // One change at a time: the first confirmed press is taken up; later
                // ones are drained (`begin_debugger_access_change` refuses them anyway).
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
        #[cfg(target_os = "macos")]
        self.apply_askpass_off_requests(askpass_requests, crate::askpass::set_relay_off);
        #[cfg(not(target_os = "macos"))]
        let _ = askpass_requests;
        // Every yes still waiting is taken up here — started, or refused and
        // said so — so no view reads it as under way after this park.
        self.owner_quiet
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .settle_confirmed();
        // A confirmed warm-up that starts nothing says why, in each view it was
        // confirmed from; one already walking is under way, which says itself.
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
                // Written outside the reducer, so the window is told to draw it.
                if noted && let Some(window) = self.windows.get_mut(&wid) {
                    window.last_present = None;
                    if let Some(os_window) = window.os_window.as_ref() {
                        os_window.request_redraw();
                    }
                }
            }
        }
        if let Some(plan) = retire {
            // The refusal, if any, is shown by the block's status row
            // (`MacosAccess::retire_status`) until the next press starts.
            self.claimant_retire_refusal = self.begin_claimant_retire(plan).err();
        }
        if let Some((change, answer)) = debugger {
            // The refusal, if any, rides the debugger row's facts
            // (`DebuggerFacts::refusal`) until the next press starts.
            let refusal = self.begin_debugger_access_change(change, answer).err();
            if let Some(why) = refusal {
                aterm_log::info!(
                    "debugger access: the confirmed change started nothing: {}",
                    why.words()
                );
            }
            self.debugger_change.note_refusal(refusal);
        }
        // The page's Open Privacy & Security… is the card's Open Settings by
        // another door: the same `opened` marker, and the same watch for the ✓.
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
        // The DEVELOPER TOOLS card's Open Developer Tools…: the one hand-over,
        // once however many views pressed it before this park, and what it did
        // on the bar of each of them. A headless instance answers that nothing
        // opened. On a list read empty the page waits for `spctl`: those bars say
        // so now, and hear how it came out when the run ends
        // (`App::settle_devtools_waiting`).
        if !devtools_by.is_empty() {
            let outcome = self.hand_devtools_to_owner(crate::devtools_helper::Origin::Settings);
            if outcome == crate::devtools_helper::HandOver::Listing {
                self.devtools_helper
                    .wait_in_settings(devtools_by.iter().copied());
            }
            self.note_devtools_outcome_on(&devtools_by, outcome, None);
        }
    }

    /// What the Developer Tools hand-over came to, on the bar of each Settings view
    /// in `views` that pressed for it (`SettingsViewState::note_devtools_hand_over`),
    /// each told to draw it: written outside the reducer. `unlisted`: why `spctl
    /// developer-mode enable-terminal` failed, when the hand-over ran it first.
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

    /// Assemble what the Security page renders: `App` state, the cached probe, and one
    /// `proc_pidpath` for the image anchor. It publishes `SpikeEvidence::MEASURED`, so
    /// each claim the panel makes stays gated on a named field. `on_screen` says whether
    /// a Security view is visible, which alone lets the Full Disk Access probe and the
    /// DEVELOPER TOOLS card dispatch.
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
            // Read fresh: it changes under a running process when an apply renames
            // the bundle away and a later launch deletes it.
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
            // A reset is reported only by the gesture that ran it, and the view keeps
            // that report across republishes (`replace_macos_access`); the host never
            // invents one, and a successor process inherits none.
            reset: None,
            claimants: claimants.map(|(census, _age)| census),
            trash_tool: crate::consent_retire::trash_tool_present(),
            retire_live: self.claimant_retire.is_live(),
            retired: self.claimant_retire.report(),
            retire_status: self.claimant_retire_status(),
            debugger: self.debugger_access_facts(),
            devtools: self.devtools_access_facts(on_screen),
            // The owner's sudo off switch, read once per projection from the
            // state-dir marker (`crate::askpass::relay_off`): the row renders
            // the right toggle. The control verb reads the marker itself.
            #[cfg(target_os = "macos")]
            askpass_relay_off: crate::askpass::relay_off(),
            #[cfg(not(target_os = "macos"))]
            askpass_relay_off: false,
        }
    }

    /// What the Security page's DEVELOPER TOOLS card renders (debugger design
    /// §9.1; owner ask 2026-10-15): the CACHED devtools slot answer — the
    /// switch's row, tccd's answer and the verdict they join to, the one the
    /// digest line and the helper panel act on — with tccd's answer aged in
    /// whole minutes so the equality-gated republish repaints at most once a
    /// minute. Only while the page is `on_screen` does a stale answer dispatch
    /// its one bounded worker off this thread (the render never waits on it);
    /// otherwise nothing is started ([`ConsentState::devtools_cached`]). While
    /// a re-read is out (a focus gain retires the answer), a switch the
    /// hand-over lane last READ stands in for it, so the card does not flicker
    /// to "Checking…" each time the owner comes back. When aterm last opened
    /// the page, and the owner's Not now while it holds, come from the lane's
    /// cached markers ([`devtools_helper::Lane::last_opened_at`],
    /// `Lane::declined_marker`): never a file read per park.
    ///
    /// [`ConsentState::devtools_cached`]: crate::consent_state::ConsentState::devtools_cached
    /// [`devtools_helper::Lane::last_opened_at`]: crate::devtools_helper::Lane::last_opened_at
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

    /// What the Security page's debugger row renders: the cached `DebuggerSlot` answer
    /// (a stale one dispatches its bounded worker off this thread; the render never
    /// waits) and the change lane's state. A completed answer is handed to the lane
    /// first, so the press guard and the page read the same observation.
    pub(crate) fn debugger_access_facts(&mut self) -> crate::debugger_access::DebuggerFacts {
        let row = self.consent.debugger(self.config.privacy_probe_gate());
        self.debugger_access_facts_from(row)
    }

    /// [`Self::debugger_access_facts`] over a row already read: a completed
    /// answer is handed to the lane first. The launch ask's look reads the
    /// cached row without dispatching while it waits for its moment.
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

    /// The Security block's *Move to Trash* status row: under way, refused before
    /// it started, or the last move's summary.
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

    /// Locate the Settings view compatibility control verbs target: the front window
    /// first, then any other live presentation of the singleton.
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

    /// Locate a live presentation of one exact Settings route for a compatibility alias
    /// in the front window's active tab. Distinct from the generic `controls prefs`
    /// target: `controls about` and `controls update` inspect the page `window
    /// about|update` photographs or report that it is not frontmost.
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

    /// Overlay-model access for the model-level tests. Production control inspection
    /// uses [`Self::native_settings_view_target`] and the native compiled semantic tree.
    #[cfg(test)]
    pub(crate) fn native_settings_legacy_state(&self) -> Option<&crate::settings::SettingsState> {
        self.native_settings_view_target()
            .map(|(_, _, _, state)| &state.legacy)
    }

    /// Select the process-singleton Settings app in the frontmost window, creating its
    /// first native tab when needed. About and Software Update are routes of this app,
    /// so they share its navigation, history, accessibility and semantic tree.
    pub(crate) fn open_settings_tab(
        &mut self,
        route: crate::native_settings::SettingsRoute,
    ) -> bool {
        let Some(wid) = self.frontmost_window else {
            return false;
        };
        self.open_settings_tab_in_window(wid, route)
    }

    /// Hand a new Settings view where aterm.toml lives, and the arm its Manual
    /// page shows the file with: inert in a headless instance, which presents
    /// no OS UI.
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

    /// [`Self::open_settings_tab`] in a named window: the message band, a screen reader's
    /// activate on it and the Messages page's actions pass the window the press landed
    /// in, so a press in a background window opens the page there and raises it.
    pub(crate) fn open_settings_tab_in_window(
        &mut self,
        wid: crate::WindowId,
        route: crate::native_settings::SettingsRoute,
    ) -> bool {
        // Singleton means one controller, not one presentation: search only the
        // requesting window, so another window never has focus stolen from it.
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
            // Attaching a view from `snapshot` is the synchronization point: its OCC
            // base and projection come from one revision before either can emit an edit.
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
            // A Settings leaf may share its tab with a focused terminal: focus the
            // exact view, not just its tab.
            let focused = ws
                .tab_set
                .active_mut()
                .is_some_and(|tab| tab.set_focus(view));
            if !focused {
                return false;
            }
            // A last-tab close parks the window behind `pending_close`; an explicit
            // reopen can race that, so cancel the stale teardown first.
            crate::app_tabs::cancel_pending_close(ws);
            ws.last_present = None;
        }
        // Publish the canonical front content (e.g. returning from Manual's config
        // Editor) before the exact-view stale-target guard resolves the action.
        self.resync_active_or_window(wid);
        self.sync_settings_consent_posture();
        self.sync_settings_session_runtime();
        if route == crate::native_settings::SettingsRoute::SoftwareUpdate {
            self.acknowledge_native_update_attention();
        }
        // Every page paints the reducer's snapshot, which learns ledger changes — and
        // whether this copy can update itself at all — only on a reconcile. Software
        // Update, Modified and Search all say what the update switches do here, and a
        // headless launch posts no startup reconcile, so ask whenever Settings opens,
        // on any page, as the Packages refresh below does; the rows correct themselves
        // when it lands (`UpdateChanged`). The work is coalesced, off the event loop.
        self.request_native_update_reconcile(
            crate::app_native::NativeUpdateReconcilePurpose::Refresh,
        );
        // Seed and refresh the shared Packages projection whenever Settings surfaces:
        // publication is memory-only (the controller starts "unobserved"), and status
        // collection runs on a worker, never on the event loop. What the package tool
        // is, is noted first, so the first publish already admits on it.
        self.note_native_package_tool();
        self.publish_native_packages_state();
        self.start_native_packages_refresh();
        // The Messages projection, unconditionally: a tab opened at the Messages route
        // reads the log as it is now, not as the 2 Hz gate last let it through.
        self.publish_native_messages_state();
        // And Settings ▸ Harness's, built from what the App holds: the route's
        // arrival (the route action below) reads the ledgers again off the loop.
        self.publish_native_harness_state();
        // Security's This Mac status reads `[machine]` from a fresh `atpkg machine`
        // every time Settings surfaces.
        self.start_native_machine_refresh();
        let action_id = crate::native_settings::route_action(route);
        let action = crate::native_app::AppEvent::Action(crate::native_app::ActionInvocation {
            id: action_id,
            value: None,
        });
        // The human/semantic-action host path honours `InvalidateOwnPresentation` and
        // `RepaintSelf`, so the retained render tray shows the new route.
        if self.dispatch_native_view_event(wid, view, action).is_err() {
            return false;
        }
        // After the route action, so a tab that just reached the Security page shows a
        // live posture immediately.
        self.sync_settings_consent_posture();
        self.sync_settings_session_runtime();
        self.sync_active_session();
        if let Some(window) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
            window.focus_window();
            window.request_redraw();
        }
        true
    }

    /// Hand the Settings view in `wid` (the one [`Self::open_settings_tab_in_window`]
    /// raised) action `id` with `value`, through the host path a person's press
    /// takes, so the reducer's repaint is honoured. Whether a view took it.
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

    /// Close every presentation of the native Settings singleton. Returns whether
    /// at least one tab was removed. The compatibility `settings false` control verb
    /// uses this exact path; no OS window or fake PTY is involved.
    pub(crate) fn close_settings_tabs(&mut self) -> bool {
        // Snapshot the targets once: a blocked draft is attempted once and stays
        // focused, so the loop cannot rediscover it and spin.
        let mut targets = Vec::new();
        for &wid in self.windows.keys() {
            self.visit_settings_tabs_in_window(wid, |_, tab, _, view| {
                targets.push((wid, tab, view));
            });
        }
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
                    // Keep the refusing view selected so its draft banner and
                    // close-recovery palette stay visible to the control caller.
                    self.sync_window(wid);
                }
            }
        }
        removed
    }

    /// Apply the compatibility `settings [open|close]` request, keeping singleton
    /// identity separate from per-window presentation. An explicit open addresses the
    /// front window; toggle and close act process-wide.
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

    /// `settings section <name>` (socket): navigate the open Settings tab to `route`
    /// (the control parser has already resolved labels and aliases). Errs when no
    /// Settings view is up.
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

    /// The voice to audition after a config swap: a changed voice (a native-window pick
    /// or hand edit) is auditioned once; re-applying the latched voice is silent.
    pub(crate) fn typing_sound_to_audition_on_swap(
        &self,
        next: &crate::app_config::Config,
    ) -> Option<aterm_effects::trail_sound::SoundVoice> {
        let next_voice = next.trail_sound_voice();
        (next_voice != self.typing_sound_auditioned).then_some(next_voice)
    }

    /// Play one keystroke of `voice` so a voice is chosen by ear: one
    /// [`aterm_effects::trail_sound::SoundKind::Typed`] cue as the loudness ladder
    /// measures a keystroke (the isolated stance: pan 0, heat 0.5, hue 0,
    /// `Tone::Technical`, no bed) at the user's volume, on the current trail look.
    /// Gated by the key-time click's predicate
    /// ([`crate::app_input::keystroke_click_audible`]: a live audio host, the "Music
    /// effects" master, non-zero volume, serious mode allowing sound), so the preview
    /// never speaks where a keystroke could not. Latches `typing_sound_auditioned`
    /// either way.
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
            // The plain keystroke, the floor the roster is compared on; a shifted probe
            // would audition every voice an octave up.
            shifted: false,
        });
    }

    /// Push the front window's open overlay accessibility tree (Settings / About /
    /// Palette / Update, or an empty root) to its native adapter. A no-op without a
    /// publisher or before its first reader attaches. Called by every
    /// overlay mutator.
    pub(crate) fn overlay_a11y_update(&mut self) {
        #[cfg(a11y_tree)]
        if let Some(wid) = self.frontmost_window {
            self.push_a11y_tree(wid);
        }
    }

    /// Build the accessibility tree for window `wid`'s open overlay (Settings / About /
    /// Palette / Update), or its grid or native tree when none is open, and hand it to
    /// the window's native accessibility adapter. Overlays fan out through
    /// [`crate::overlay::OverlayModel::a11y`] off the same model the pixels and
    /// `controls` verb read.
    ///
    /// Returns before building anything unless an OS a11y client has attached
    /// ([`crate::WindowState::a11y_active`]); attachment arrives as
    /// `InitialTreeRequested`, which raises the latch and then publishes here.
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
        if self
            .windows
            .get(&wid)
            .is_none_or(|ws| ws.a11y.is_none() || !ws.a11y_active)
        {
            return;
        }
        // With no overlay open, publish the live terminal grid (visible text, read-only),
        // snapshotted before the mutable window borrow below.
        let has_overlay = self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.overlay.is_some());
        // A native frame stages its projection from the CompiledUi used for paint; an
        // initial-tree request before first paint compiles one here. `None` means the
        // front content is terminal; `Some(Err)` is a native projection failure and fails
        // closed rather than announce a hidden terminal grid.
        let native_update = (!has_overlay)
            .then(|| match frame_plan {
                Some(plan) => self.take_native_accessibility_update_from_plan(wid, plan),
                None => self.take_native_accessibility_update(wid),
            })
            .flatten();
        // The terminal grid's tree, built before the mutable window borrow below (both
        // borrow `self`) and only where neither an overlay nor a native view owns it.
        let grid_update = (!has_overlay && native_update.is_none())
            .then(|| self.grid_a11y_tree(wid, frame_plan))
            .flatten();
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
        let Some(adapter) = ws.a11y.as_mut() else {
            return;
        };
        adapter.update_if_active(move || update);
    }

    /// THE TERMINAL GRID'S TREE for window `wid` (`a11y_tree::grid_tree`), as
    /// [`Self::push_a11y_tree_with_plan`] publishes it where no overlay or native view
    /// owns the window: the visible rows, and everything the frame paints over them that
    /// a reader must meet — the tab strip, split panes, the painted selection, the find
    /// field, the chrome messages, the harness's chips and the open Autopilot menu
    /// (design §9.2.1, §9.3.3). Every input is read from state the frame already
    /// computed (the presented `input_scratch`, the hits and the rect the last paint
    /// recorded), so a reader is told the frame on the glass and no layout work is
    /// added. `None` for an unknown window.
    #[cfg(a11y_tree)]
    pub(crate) fn grid_a11y_tree(
        &self,
        wid: crate::WindowId,
        frame_plan: Option<&crate::tab_model::VisibleLeafPlan>,
    ) -> Option<aterm_a11y::model::TreeUpdate> {
        // Split panes, painted selection and the find field, all read from the same
        // presented `input_scratch` as the rows, so the reader is told the frame on the
        // glass. Compute `panes` before `grid_snapshot` so a split-pane frame skips
        // scanning all `rows * cols` into a full-screen string that `grid_tree` discards.
        let initial_plan = frame_plan
            .is_none()
            .then(|| self.active_visible_leaf_plan(wid))
            .flatten();
        let panes = frame_plan
            .or(initial_plan.as_ref())
            .map(|plan| self.grid_a11y_panes(wid, plan))
            .unwrap_or_default();
        let snap = if !panes.is_empty() {
            self.windows
                .get(&wid)
                .filter(|ws| !ws.input_scratch.cells.is_empty())
                .map(|ws| {
                    let strip = usize::from(self.chrome_rows(wid));
                    crate::accessibility::AccessibleSnapshot {
                        text: String::new(),
                        rows: ws.input_scratch.cells.len().saturating_sub(strip),
                        cols: usize::from(ws.cols),
                        cursor: None,
                    }
                })?
        } else {
            self.grid_snapshot(wid)?
        };
        // The grid's on-screen geometry and its tab strip, both read from state the frame
        // already computed (`win_cell_size`/`win_pad_top`, and the `tab_segments` the mouse
        // hit-tests), so no layout work is added.
        let geometry = self.grid_a11y_geometry(wid);
        let tabs = self.grid_a11y_tabs(wid);
        let selection = panes
            .is_empty()
            .then(|| self.grid_a11y_selection(wid, &snap, 0, 0..snap.cols))
            .flatten();
        let find = self.grid_a11y_find(wid);
        // Chrome messages, for the grid tree only: an overlay or native tab app owns the
        // whole tree while up, and the bands are not on its glass.
        let messages = self.grid_a11y_messages(wid);
        // The harness's chips in the frame's composer rules and Codex's chip rows (design
        // §9.2.1): glass the grid's text never holds, read off the hits the last paint
        // recorded, so a reader is told exactly the chips a pointer can press.
        let chips = self.claude_chips_a11y(wid);
        // The open Autopilot menu (design §9.3.3), where the last paint drew it: a menu
        // of items in words, the highlighted row the tree's focus.
        let menu = self.autopilot_grid_menu(wid);
        // The stats bars (the Deck or the sill): groups of meters, the core row a grid.
        let stats = self.stats_a11y(wid);
        // The window's chrome title, so the accessible window is named as the titlebar.
        let window_title = self
            .windows
            .get(&wid)
            .map(|ws| ws.current_title.as_str())
            .unwrap_or_default();
        Some(crate::a11y_tree::grid_tree(
            &snap,
            geometry,
            &crate::a11y_tree::GridFrame {
                tabs: &tabs,
                panes: &panes,
                selection,
                find: find.as_ref(),
                messages: &messages,
                chips: &chips,
                menu: menu.as_ref(),
                stats: stats.as_ref(),
                window_title,
            },
        ))
    }

    /// Where window `wid`'s visible grid (and the tab strip above it) sits in the client
    /// area, in physical pixels (the space the AT-SPI adapter reports root bounds in),
    /// from the window's own metrics. `None` for an unknown window or degenerate cell
    /// size; the tree then omits geometry (the Text interface is unaffected).
    #[cfg(a11y_tree)]
    fn grid_a11y_geometry(&self, wid: crate::WindowId) -> Option<crate::a11y_tree::GridGeometry> {
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
        // The strip starts below the top pad and chrome headroom; the grid starts below
        // the strip (`native_content_origin_y`, where `grid_snapshot` re-bases the cursor).
        let strip_y = self.win_pad_top(wid).saturating_add(self.win_head(wid));
        Some(crate::a11y_tree::GridGeometry {
            origin_x: self.win_pad(wid) as f64,
            origin_y: self.native_content_origin_y(wid) as f64,
            cell_w: cw as f64,
            cell_h: ch as f64,
            strip_y: strip_y as f64,
            strip_h: (strip_rows * ch) as f64,
            // The bar band sits between the strip and grid row 0 — the same
            // arithmetic `native_content_origin_y` uses, from the other end.
            bars_y: (strip_y + strip_rows * ch) as f64,
            // One row tall whenever a band row is up (presence owns no row).
            bar_h: if self.message_band_rows == 0 {
                0.0
            } else {
                ch as f64
            },
        })
    }

    /// The in-grid tab strip of window `wid` as publishable a11y items, or empty.
    ///
    /// Empty unless the strip is on screen (`tab_strip_rows > 0`) with at least two tabs:
    /// a solo strip paints the window title (`TabSegment::solo`). Labels come from
    /// `strip_titles_scratch` and spans from the cached `tab_segments`, the buffers the
    /// last paint filled and the mouse hit-tests, so announced and clickable tabs agree.
    /// The label is announced whole, not as the chip's cut
    /// (`chrome_label::capped_label`).
    #[cfg(a11y_tree)]
    fn grid_a11y_tabs(&self, wid: crate::WindowId) -> Vec<crate::a11y_tree::GridTab> {
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
                Some(crate::a11y_tree::GridTab {
                    index,
                    id: ws.tab_set.tab_at(index)?.id.get(),
                    title: ws.strip_titles_scratch.get(index).map_or_else(
                        || "aterm".to_string(),
                        |label| crate::chrome_label::capped_label(label).into_owned(),
                    ),
                    selected: active == Some(index),
                    start_col: segment.start_col,
                    end_col: segment.end_col,
                    // The tab's focused session's own theme, by the selector it
                    // asks for (no resolution on the a11y path).
                    theme: ws
                        .layouts
                        .get(index)
                        .and_then(|tree| self.session_choice(tree.focus()))
                        .map(|choice| choice.selector),
                })
            })
            .collect()
    }

    /// Window `wid`'s visible split panes as one terminal document each, sliced from the
    /// composed cells the frame was drawn from. Empty for one pane or a zoomed tab. A
    /// pane's slot is its layout position, so closing a pane renumbers the rest.
    #[cfg(a11y_tree)]
    fn grid_a11y_panes(
        &self,
        wid: crate::WindowId,
        plan: &crate::tab_model::VisibleLeafPlan,
    ) -> Vec<crate::a11y_tree::GridPane> {
        if plan.leaves.len() < 2 || plan.zoomed {
            return Vec::new();
        }
        let Some(ws) = self.windows.get(&wid) else {
            return Vec::new();
        };
        let strip = usize::from(self.chrome_rows(wid));
        let total = plan.leaves.len();
        plan.leaves
            .iter()
            .enumerate()
            .map(|(slot, leaf)| {
                self.view_store
                    .get(leaf.view)
                    .copied()
                    .and_then(crate::tab_model::View::terminal_session)?;
                let row_off = leaf.rect.origin.y.round().max(0.0) as usize;
                let col_off = leaf.rect.origin.x.round().max(0.0) as usize;
                let rows = (leaf.rect.size.height.round() as usize).max(1);
                let cols = (leaf.rect.size.width.round() as usize).max(1);
                // The composed frame carries one cursor, the focused pane's; a sibling
                // publishes none rather than borrowing it.
                let cursor = (ws.input_scratch.cursor_visible && leaf.focused)
                    .then(|| {
                        let crow = ws.input_scratch.cursor_row.saturating_sub(strip);
                        let ccol = ws.input_scratch.cursor_col;
                        (crow >= row_off
                            && crow < row_off + rows
                            && ccol >= col_off
                            && ccol < col_off + cols)
                            .then_some((crow - row_off, ccol - col_off))
                    })
                    .flatten();
                let snap = crate::accessibility::AccessibleSnapshot::from_cells_in(
                    &ws.input_scratch.cells,
                    strip + row_off..strip + row_off + rows,
                    col_off..col_off + cols,
                    cursor,
                );
                let selection =
                    self.grid_a11y_selection(wid, &snap, row_off, col_off..col_off + cols);
                Some(crate::a11y_tree::GridPane {
                    slot,
                    label: format!(
                        "{} — pane {} of {total}",
                        crate::accessibility::LABEL,
                        slot + 1
                    ),
                    snap,
                    row_off,
                    col_off,
                    focused: leaf.focused,
                    selection,
                })
            })
            .collect::<Option<Vec<_>>>()
            .unwrap_or_default()
    }

    /// The painted selection covering one published document: the whole visible grid
    /// (`row_off = 0`, full column range) or one pane's rectangle. Read from the
    /// renderer's per-row, per-entry `RenderInput::selection_row_key`, which keeps a
    /// split's panes apart (`selection_row_span`'s hull would span the divider). Paint
    /// snaps a wide glyph whole and this does not, so an edge on a wide lead can differ
    /// by one cell. `None` for no selection or one that clips to a point, so a click
    /// that selected nothing leaves the caret.
    #[cfg(a11y_tree)]
    fn grid_a11y_selection(
        &self,
        wid: crate::WindowId,
        snap: &crate::accessibility::AccessibleSnapshot,
        row_off: usize,
        cols: std::ops::Range<usize>,
    ) -> Option<crate::a11y_tree::GridSpan> {
        let ws = self.windows.get(&wid)?;
        let strip = usize::from(self.chrome_rows(wid));
        let mut keys: Vec<aterm_core::render::SelectionRowKey> = Vec::new();
        let mut anchor: Option<(usize, usize)> = None;
        let mut focus: Option<(usize, usize)> = None;
        for (row, line) in snap.value().split_inclusive('\n').enumerate() {
            ws.input_scratch
                .selection_row_key(strip + row_off + row, &mut keys);
            // Clip each entry to this document's columns and take the hull: within one
            // pane's box entries cannot overlap, and a single document has only one.
            let Some((lo, hi)) = keys
                .iter()
                .filter_map(|k| {
                    let lo = usize::from(k.lo).max(cols.start);
                    let hi = usize::from(k.hi).min(cols.end.saturating_sub(1));
                    (lo <= hi).then_some((lo, hi))
                })
                .reduce(|(a_lo, a_hi), (b_lo, b_hi)| (a_lo.min(b_lo), a_hi.max(b_hi)))
            else {
                continue;
            };
            // The published text has this row's trailing blanks trimmed, so a span
            // running into them ends at the row's last real character.
            let trimmed = line.strip_suffix('\n').unwrap_or(line).chars().count();
            let start = lo.saturating_sub(cols.start).min(trimmed);
            let end = (hi.saturating_sub(cols.start) + 1).min(trimmed);
            anchor.get_or_insert((row, start));
            focus = Some((row, end));
        }
        let (anchor, focus) = (anchor?, focus?);
        (anchor != focus).then_some(crate::a11y_tree::GridSpan { anchor, focus })
    }

    /// The find panel's query field for window `wid`, or `None` when find mode is off.
    /// The panel paints as terminal cells; this node makes it a field: the query, the
    /// caret, and the panel's answer plus active modes as the description.
    #[cfg(a11y_tree)]
    fn grid_a11y_find(&self, wid: crate::WindowId) -> Option<crate::a11y_tree::GridFind> {
        let search = self.windows.get(&wid)?.search.as_ref()?;
        // `SearchState::cursor` is a byte offset on a char boundary; the a11y caret is a
        // character index into the same string.
        let caret = search
            .query
            .get(..search.cursor)
            .map_or_else(|| search.query.chars().count(), |head| head.chars().count());
        let mut status: Vec<String> = search.spoken_status().into_iter().collect();
        if search.case_sensitive {
            status.push("case sensitive".to_string());
        }
        if search.is_regex {
            status.push("regular expression".to_string());
        }
        Some(crate::a11y_tree::GridFind {
            query: search.query.clone(),
            caret,
            status: status.join("; "),
        })
    }

    /// The chrome messages window `wid` is showing now: surfaces that speak unprompted
    /// and then leave, which a screen reader cannot find by exploring. Their words reach
    /// the grid document as anonymous cells; this adds a node per row with its role,
    /// capsules and place.
    ///
    /// Every entry is gated on the same predicate as the pixels (the committed
    /// `message_band_rows`), so a message is announced exactly when it is on this
    /// window's glass.
    ///
    /// A band row speaks `title[ · detail[0]]`, its first detail line whole and only
    /// where the row paints it as its excerpt, and leaves the rest to the Details press.
    /// Pictographic joints are spoken as words (`aterm_messages::glass::speakable`: `→`
    /// as "did you mean", `×` between numbers as "by", `▸` as a pause). The spoken
    /// sentence is width-independent (a resize never re-announces) and changes only with
    /// the work's phase; `stats` ride the description with the capsules' full labels,
    /// so a live region never floods.
    ///
    /// A band row speaks `title[ · detail[0]]` (the detail only where the row paints
    /// it), with pictographic joints spoken as words (`glass::speakable` in
    /// aterm-messages). The sentence is width-independent and changes only with the
    /// work's phase; `stats` ride the description, so a live region never floods. A row
    /// that asks is a polite alert, like a failure. Robi's tip bubble is never announced.
    #[cfg(a11y_tree)]
    fn grid_a11y_messages(&self, wid: crate::WindowId) -> Vec<crate::a11y_tree::GridMessage> {
        use crate::a11y_tree::{ChromeMessage, GridMessage};
        let Some(ws) = self.windows.get(&wid) else {
            return Vec::new();
        };
        let mut out: Vec<GridMessage> = Vec::new();

        // The needs pill first: the window's most pressing need, said as the pill
        // says it, a status with no rectangle of its own among the bars (the pill
        // is titlebar chrome, owner surfaces §5).
        if let Some(message) = self.presence_a11y_message(wid) {
            out.push(message);
        }

        // The band rows, top to bottom, but only the ones whose rows the geometry has
        // actually committed — `splice_message_band` trims its painted cache to that
        // count too, so the tree can never describe a row the frame does not carry.
        // Fixed slots per band ROW (`BandRow0..2`), not per message: the publisher then
        // emits "text changed" on a name change instead of node churn.
        let presentation = if self.message_band_rows > 0 {
            Some(self.band_presentation(usize::from(ws.cols)))
        } else {
            None
        };
        for (row, layout) in presentation
            .iter()
            .flat_map(|p| p.rows.iter())
            .take(usize::from(self.message_band_rows))
            .enumerate()
        {
            let bar_row = row;
            let capsules: Vec<crate::a11y_tree::BandCapsule> = layout
                .capsules
                .iter()
                // A blanked capsule (an echo's spent `Stop paste`)
                // keeps its cells and has nothing to say.
                .filter(|c| !c.full_label.is_empty())
                .map(|c| crate::a11y_tree::BandCapsule {
                    label: c.full_label,
                    col: c.col,
                    width: c.width,
                    index: crate::a11y_tree::capsule_index(c.action),
                })
                .collect();
            match layout.kind {
                aterm_messages::RowKind::Message(id) => {
                    let Some(live) = self.messages.live(id) else {
                        continue;
                    };
                    let Some(message) = ChromeMessage::band_row(row) else {
                        continue;
                    };
                    // The description, read on demand and never announced: time words,
                    // load words, stats, then the capsules' full labels.
                    let now = std::time::Instant::now();
                    let busy = live.activity() == 2;
                    // The work's own clock, the one the band's elapsed words read, so
                    // the description agrees with the paint: a busy row, and any row
                    // the layout gives an elapsed slot, says how long it has run; a determinate
                    // row says what is left, and nothing while its estimate is hidden
                    // (its slot is blank then).
                    let ran = now.saturating_duration_since(live.started_at);
                    let time_words = if busy || layout.elapsed.is_some() {
                        aterm_messages::progress::elapsed_spoken(ran)
                    } else {
                        aterm_messages::progress::eta_spoken(live.track.eta(now))
                    };
                    let stats = live
                        .msg
                        .meter
                        .as_ref()
                        // Unpadded: the right-alignment that keeps the
                        // painted digits in place is not something to read.
                        .map(|m| m.stats.trim().to_string())
                        .filter(|s| !s.is_empty());
                    let labels = layout
                        .capsules
                        .iter()
                        .map(|c| c.full_label)
                        .collect::<Vec<_>>()
                        .join(" \u{00b7} ");
                    let detail = [
                        time_words,
                        layout.load.map(|(_, words)| words.to_string()),
                        stats,
                        Some(labels).filter(|l| !l.is_empty()),
                    ]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join("; ");
                    // The name: the title, and `detail[0]` only where the row shows it
                    // as its excerpt; width- and time-free, so a countdown never
                    // re-announces the row.
                    let detail0 = if live.msg.excerpt {
                        live.msg.detail.first().map_or("", String::as_str)
                    } else {
                        ""
                    };
                    out.push(GridMessage {
                        message,
                        context: live.action_revision().map(|revision| {
                            aterm_a11y::model::ActionContext::message(id.raw(), revision)
                        }),
                        text: layout.spoken(detail0),
                        detail: Some(detail),
                        busy,
                        progress: live
                            .msg
                            .meter
                            .as_ref()
                            .and_then(|m| m.fill_permille)
                            .map(|f| f32::from(f) / 1000.0),
                        // A failure or a question is an alert.
                        alarm: matches!(
                            live.msg.severity,
                            aterm_messages::Severity::Warn | aterm_messages::Severity::Error
                        ) || live.msg.is_ask()
                            || live
                                .msg
                                .actions
                                .iter()
                                .any(aterm_messages::Intent::is_consequential),
                        // A body press or a screen reader's activate is the row's
                        // `Details ›` (`App::press_message_body`); each capsule is its
                        // own button.
                        activates: true,
                        bar_row: Some(bar_row),
                        capsules,
                    });
                }
                // An echo is the band's end of its own indicator: not a row, not
                // pressable, not announced.
                aterm_messages::RowKind::Echo(_) => {}
                aterm_messages::RowKind::Overflow { .. } => {
                    out.push(GridMessage {
                        message: ChromeMessage::BandOverflow,
                        context: Some(aterm_a11y::model::ActionContext::message_log()),
                        text: layout.full_title.clone(),
                        detail: None,
                        busy: false,
                        progress: None,
                        alarm: false,
                        // One link, to Settings ▸ Messages at the top of the log: an
                        // activate on it opens the page, like a press anywhere on it.
                        activates: true,
                        bar_row: Some(bar_row),
                        // The node is the link: its words are the whole row, and the
                        // tree publishes capsule children only under the three
                        // message-row slots.
                        capsules: Vec::new(),
                    });
                }
            }
        }

        // The multi-line-paste confirmation, assertive (`ChromeMessage::politeness`)
        // because nothing reaches the PTY until it is answered; the risk and the answer
        // keys are the description.
        if let Some(pending) = self.paste_banner.as_ref().filter(|p| p.wid == wid) {
            out.push(GridMessage {
                message: ChromeMessage::PasteConfirm,
                context: None,
                text: crate::paste_banner::question(pending.text()),
                detail: Some(crate::paste_banner::announcement_detail()),
                busy: false,
                progress: None,
                alarm: false,
                // Enter and Escape answer it. A screen reader's Click carries neither
                // meaning, and guessing one on a security prompt is the wrong default.
                activates: false,
                // The band overwrites the frame's top rows rather than reserving a row of
                // its own, so it has no rectangle of its own to publish.
                bar_row: None,
                capsules: Vec::new(),
            });
        }

        // The close/quit confirmation (Windows, `close_confirm`): an assertive alert
        // dialog with its two answer keys as the description.
        if let Some(pending) = self.close_banner.as_ref().filter(|p| p.wid == wid) {
            out.push(GridMessage {
                message: ChromeMessage::CloseConfirm,
                context: None,
                text: crate::close_confirm::question(&pending.prompt),
                detail: Some(crate::close_confirm::answer_keys(&pending.prompt)),
                busy: false,
                progress: None,
                alarm: false,
                activates: false,
                // Like the paste band, it overwrites the frame's top rows.
                bar_row: None,
                capsules: Vec::new(),
            });
        }
        out
    }

    /// A screen reader activated one of window `wid`'s chrome messages.
    ///
    /// The request names a node from a tree published frames ago, so every arm asks the
    /// same live question the pointer path asks before acting, and an activate can never
    /// reach a surface a click could not.
    #[cfg(a11y_tree)]
    fn activate_a11y_message(
        &mut self,
        wid: crate::WindowId,
        message: crate::a11y_tree::ChromeMessage,
        context: Option<aterm_a11y::model::ActionContext>,
    ) {
        use crate::a11y_tree::ChromeMessage;
        match message {
            // The pill's status informs and never activates (`activates: false`);
            // its acts are the pill's own, and a stale activate is a no-op.
            ChromeMessage::PresenceStatus => {}
            // As the band's click route (`App::press_band`): a row's activate is its body
            // press (`Details ›`, via `App::press_message_body`), a capsule's is that
            // capsule. Re-resolved at this window's width and gated on the row still being
            // committed, so an activate on a folded row does nothing.
            ChromeMessage::BandRow0 | ChromeMessage::BandRow1 | ChromeMessage::BandRow2 => {
                if let Some(id) =
                    self.committed_a11y_message(wid, message.band_row_index(), context)
                {
                    self.press_message_body(wid, id);
                }
            }
            ChromeMessage::BandCapsule { row, index } => {
                let Some(id) = self.committed_a11y_message(wid, Some(usize::from(row)), context)
                else {
                    return;
                };
                let action = crate::a11y_tree::capsule_action(index);
                if action.is_details() {
                    self.press_message_body(wid, id);
                    return;
                }
                let Some(context) = context else {
                    return;
                };
                let intent = self.messages.act_at_revision(
                    id,
                    context.generation(),
                    action,
                    std::time::Instant::now(),
                );
                self.sync_messages();
                if let Some(intent) = intent {
                    let _ = self.perform_intent(wid, id, intent);
                }
            }
            // The overflow row is one link (Settings ▸ Messages, top of the log, in this
            // window), gated on the band still carrying an overflow row.
            ChromeMessage::BandOverflow => {
                if context != Some(aterm_a11y::model::ActionContext::message_log()) {
                    return;
                }
                let cols = self.windows.get(&wid).map(|ws| usize::from(ws.cols));
                let overflow = cols.is_some_and(|cols| {
                    self.band_presentation(cols)
                        .rows
                        .iter()
                        .take(usize::from(self.message_band_rows))
                        .any(|row| matches!(row.kind, aterm_messages::RowKind::Overflow { .. }))
                });
                if overflow {
                    let _ = self.open_messages_entry(wid, None);
                }
            }
            // No Click in the published tree: the paste and close questions are answered
            // by Enter/Escape, and a generic activate must not guess a security or
            // destructive answer.
            ChromeMessage::PasteConfirm | ChromeMessage::CloseConfirm => {}
        }
    }

    /// A committed row still has the id and action authorization captured by
    /// the publisher. Reject absent, stale and other-domain contexts.
    #[cfg(a11y_tree)]
    fn committed_a11y_message(
        &self,
        wid: crate::WindowId,
        row: Option<usize>,
        context: Option<aterm_a11y::model::ActionContext>,
    ) -> Option<aterm_messages::MessageId> {
        let id = self.committed_band_message(wid, row)?;
        let revision = self.messages.live(id)?.action_revision()?;
        let expected = aterm_a11y::model::ActionContext::message(id.raw(), revision);
        (context == Some(expected)).then_some(id)
    }

    /// The message on committed band row `row` of window `wid` now, or `None` when the
    /// row is not committed, is the overflow link, or holds no message: the live
    /// re-check every a11y activate makes.
    #[cfg(a11y_tree)]
    fn committed_band_message(
        &self,
        wid: crate::WindowId,
        row: Option<usize>,
    ) -> Option<aterm_messages::MessageId> {
        let row = row?;
        if row >= usize::from(self.message_band_rows) {
            return None;
        }
        let cols = usize::from(self.windows.get(&wid)?.cols);
        match self.band_presentation(cols).rows.get(row)?.kind {
            aterm_messages::RowKind::Message(id) => self.messages.live(id).map(|l| l.id),
            aterm_messages::RowKind::Echo(_) | aterm_messages::RowKind::Overflow { .. } => None,
        }
    }

    /// Handle an event from a window's native accessibility adapter (delivered as
    /// `Wake::Accessibility`): the OS a11y client requesting the initial tree, an action
    /// request from a screen reader, or deactivation.
    #[cfg(a11y_tree)]
    pub(crate) fn on_accessibility_event(&mut self, event: crate::a11y_adapter::Event) {
        use crate::a11y_adapter::WindowEvent as AkWindowEvent;
        let Some(wid) = self.winit_to_window.get(&event.window_id).copied() else {
            return;
        };
        match event.window_event {
            // The OS a11y client is initialising: hand it the live tree. This is the only
            // edge that says someone is listening, so raise the latch first so this
            // publish and every later present build a tree.
            AkWindowEvent::InitialTreeRequested => {
                crate::a11y_backend::note_at_client();
                if let Some(ws) = self.windows.get_mut(&wid) {
                    ws.a11y_active = true;
                }
                self.push_a11y_tree(wid);
            }
            // A screen reader activated or focused a control.
            AkWindowEvent::ActionRequested(req) => self.on_accessibility_action(wid, req),
            // The client detached, or the accessibility bus was lost: drop the latch so
            // presents stop building trees. A re-attach re-fires `InitialTreeRequested`.
            AkWindowEvent::AccessibilityDeactivated => {
                if let Some(ws) = self.windows.get_mut(&wid) {
                    ws.a11y_active = false;
                }
            }
        }
    }

    /// Route an OS accessibility `ActionRequest` on `wid`. With an overlay up it goes to
    /// that overlay, decoded with the id scheme its `a11y()` builder minted (a stale
    /// epoch, or a disabled row's Click, decodes to nothing); otherwise to the grid's
    /// tab and chrome-message nodes, then the native tab app.
    #[cfg(a11y_tree)]
    pub(crate) fn on_accessibility_action(
        &mut self,
        wid: crate::WindowId,
        req: aterm_a11y::model::ActionRequest,
    ) {
        use crate::overlay::OverlayKind;
        // Screen-reader actions bypass keyboard/pointer ingress, so even an ignored or
        // stale request is a newer external-input boundary and closes the cursor-move
        // licence before any early return.
        self.clear_move_license(wid);
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
                if self.tab_strip_rows == 0 || ws.tab_set.len() < 2 {
                    return None;
                }
                ws.tab_set.tab_at(index).map(|tab| tab.id.get())
            }) {
                self.switch_tab_in(wid, index);
                if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
                    w.request_redraw();
                }
                // Republish synchronously: a screen reader must hear the new selection
                // even if the visual frame is coalesced away.
                self.push_a11y_tree(wid);
                return;
            }
            // A row of the open Autopilot menu the reader clicked: what a click on that
            // row does, while the rows it was published from (its id's epoch) are the
            // painted ones.
            if req.action == aterm_a11y::model::Action::Click
                && let Some((epoch, item)) = crate::a11y_tree::menu_item_for(req.target_node)
            {
                self.choose_autopilot_menu_item_for_reader(wid, epoch, item);
                return;
            }
            // The Autopilot chip's menu, asked for (design §9.3.3: what its right-click
            // does), opened as from the keyboard while the chip is on the glass.
            if req.action == aterm_a11y::model::Action::ShowContextMenu
                && let Some((session, crate::a11y_tree::HarnessChip::Autopilot)) =
                    crate::a11y_tree::chip_for(req.target_node)
            {
                self.open_autopilot_menu_for_reader(wid, session);
                return;
            }
            // A harness chip the reader clicked: what a click on its cells does, while
            // it is still on the glass (a stale request is a no-op).
            if req.action == aterm_a11y::model::Action::Click
                && let Some((session, chip)) = crate::a11y_tree::chip_for(req.target_node)
            {
                self.press_claude_chip_for_reader(wid, session, chip);
                return;
            }
            // A chrome message the reader activated; its id range is its own (below the
            // tab range `tab_index_for` refuses).
            if req.action == aterm_a11y::model::Action::Click
                && let Some(message) = crate::a11y_tree::ChromeMessage::from_node(req.target_node)
            {
                if req.target_tree == aterm_a11y::model::TreeId::ROOT {
                    self.activate_a11y_message(wid, message, req.context);
                }
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
                        self.palette_activate();
                    }
                    _ => {}
                }
            }
            // The session picker, with the palette's id scheme: Focus moves the cursor,
            // Click chooses; stale-epoch nodes decode to nothing.
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
                    aterm_a11y::model::Action::Focus => {
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
                    aterm_a11y::model::Action::Click => {
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
            // The Overview, with the palette's id scheme over its items: Focus
            // moves the cursor, Click selects then activates (a chip goes there,
            // a manages line to its manager; activation never stops a worker:
            // an armed Stop Worker confirm is resolved only by a second ⌫).
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
mod tests {
    use std::time::{Duration, Instant};

    use crate::App;
    use crate::native_app::{ActionInvocation, AppEffect, AppEvent, SemanticInput};
    use crate::native_ui::ActionId;
    use aterm_effects::cursor_glow::GlowStyle;
    use aterm_effects::trail_sound::{SoundGesture, SoundKind, SoundVoice};

    /// THE RUNNING APP REUSES A SHORT WINDOW'S CHROME BETWEEN ITS FRAMES
    /// (#64): the pass the event loop runs before every wait hands each
    /// Settings view the facts it shows (its session runtime, the font list,
    /// the faces drawn, the access posture), and a frame of the preview's
    /// clock compiles the page again. Neither changes what the page shows,
    /// so neither moves the view's content revision, nor does a pointer that
    /// moves and stays over what it was over, and every such frame takes the
    /// chrome the page last decided instead of seating the page under each
    /// rung of its ladder. The pass took the view's state to change on every
    /// wait, and the pointer on every move, which moved the revision, so the
    /// running app decided again every frame.
    #[test]
    fn an_idle_pass_and_a_frame_of_the_clock_keep_the_chrome_decided() {
        use crate::native_app::AppViewState;
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        // Top Settings at 150x24: its page sheds chrome, its preview moves.
        app.on_resize(wid, aterm_window::dpi::PhysicalSize::new(1_078, 360));
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (_, view) = app.active_native_view(wid).expect("Settings active");
        let content = |app: &App| match app.native_runtime.view_state(view) {
            Some(AppViewState::Settings(state)) => state.common.content_epoch,
            _ => unreachable!(),
        };
        let remembered = || crate::native_settings::CHROME_REMEMBERED.with(std::cell::Cell::get);
        // What the first frames settle (the font list landing, the faces).
        for _ in 0..3 {
            app.sync_settings_views();
            let _ = app.compiled_native_ui(wid).expect("a page");
        }
        let (before, decided) = (content(&app), remembered());
        for step in 0..4 {
            app.sync_settings_views();
            // The pointer moves over the page's backdrop, over nothing.
            app.on_cursor_moved(wid, 4.0 + f64::from(step), 4.0);
            let _ = app.compiled_native_ui(wid).expect("a page");
        }
        assert_eq!(content(&app), before, "nothing the page shows changed");
        assert_eq!(
            remembered() - decided,
            4,
            "each frame took the chrome decided"
        );
    }

    #[test]
    fn settings_session_runtime_is_published_from_local_state() {
        let mut app = App::headless_for_test();
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::WindowTabs));
        let (_, view) = app.active_native_view(crate::WindowId(0)).unwrap();
        let expected = crate::native_settings::SessionRuntime {
            desk: app.desk,
            journal: app.session_log_journal.status(),
            keeper: crate::keeper_link::status(),
        };
        let Some(crate::native_app::AppViewState::Settings(state)) =
            app.native_runtime.view_state_mut(view)
        else {
            unreachable!()
        };
        state.session_runtime = None;
        app.sync_settings_session_runtime();
        let Some(crate::native_app::AppViewState::Settings(state)) =
            app.native_runtime.view_state(view)
        else {
            unreachable!()
        };
        assert_eq!(state.session_runtime, Some(expected));
        assert_eq!(
            expected.journal.phase, None,
            "headless fixture starts no writer"
        );
    }

    #[test]
    fn askpass_settings_host_reports_a_failed_private_marker_operation() {
        let dir = aterm_tempfile::TempDir::new().unwrap();
        let marker = dir.path().join("askpass-off");
        std::fs::create_dir(&marker).unwrap();
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Security));
        let (_, view) = app.active_native_view(wid).unwrap();
        app.apply_askpass_off_requests(
            vec![(
                wid,
                view,
                false,
                crate::native_settings::OwnerYes::for_test(),
            )],
            |off, _owner| {
                assert!(!off);
                std::fs::remove_file(&marker)
            },
        );
        let Some(crate::native_app::AppViewState::Settings(state)) =
            app.native_runtime.view_state(view)
        else {
            unreachable!();
        };
        let said = state.feedback.as_deref().unwrap();
        assert!(said.starts_with("Could not change"), "{said}");
        assert!(!said.contains("allowed again"), "{said}");
        assert!(marker.is_dir(), "the relay still has its off entry");
    }

    #[test]
    fn askpass_settings_host_supersedes_a_second_direction_without_reporting_it_saved() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Security));
        let (instance, first) = app.active_native_view(wid).unwrap();
        let second = app.view_store.insert_native(instance).unwrap();
        let mut state =
            crate::native_settings::SettingsViewState::new(&crate::app_config::Config::default());
        state.navigate(crate::native_settings::SettingsRoute::Security);
        app.native_runtime
            .attach_view(
                second,
                instance,
                crate::native_app::AppViewState::Settings(Box::new(state)),
            )
            .unwrap();
        let mut writes = Vec::new();
        app.apply_askpass_off_requests(
            vec![
                (
                    wid,
                    first,
                    true,
                    crate::native_settings::OwnerYes::for_test(),
                ),
                (
                    wid,
                    second,
                    false,
                    crate::native_settings::OwnerYes::for_test(),
                ),
            ],
            |off, _owner| {
                writes.push(off);
                Ok(())
            },
        );
        assert_eq!(writes, vec![true], "only the first confirmed request runs");
        let Some(crate::native_app::AppViewState::Settings(first)) =
            app.native_runtime.view_state(first)
        else {
            unreachable!();
        };
        assert!(first.feedback.as_deref().unwrap().contains("turned off"));
        let Some(crate::native_app::AppViewState::Settings(second)) =
            app.native_runtime.view_state(second)
        else {
            unreachable!();
        };
        let said = second.feedback.as_deref().unwrap();
        assert!(said.contains("this request did not run"), "{said}");
        assert!(!said.contains("allowed again"), "{said}");
    }

    #[derive(Clone, Copy, Debug)]
    enum CursorDependent {
        Rainbow,
        Droplet,
        BeamRod,
        Comet,
        Phaser,
        Trail,
        Cat,
        WordDecoration,
    }

    const CURSOR_DEPENDENTS: [CursorDependent; 8] = [
        CursorDependent::Rainbow,
        CursorDependent::Droplet,
        CursorDependent::BeamRod,
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
        let geom = crate::geom_24x80_for_test();
        let cur = Some((2, 4));
        let mut quads = Vec::new();
        match effect {
            CursorDependent::Rainbow => {
                ws.fx.cursor_mut().rainbow.tick(
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
                        base: None,
                        head_rgb: None,
                        paint: None,
                        ground: None,
                        flare_at: None,
                    },
                    &mut quads,
                );
                assert!(ws.fx.cursor_mut().rainbow.is_active());
            }
            CursorDependent::Droplet => {
                ws.fx.cursor_mut().droplet.tick(
                    cur,
                    now,
                    1.0,
                    geom,
                    &crate::cursor_droplet::DropletConfig {
                        enabled: true,
                        intensity: 1.0,
                        ground: None,
                    },
                    &mut quads,
                );
                assert!(ws.fx.cursor_mut().droplet.is_active());
            }
            CursorDependent::BeamRod => {
                ws.fx.cursor_mut().beamrod.tick(
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
                        dark_theme: true,
                        theme_fg: 0x00C8_D3F5,
                        theme_bg: 0x001A_1B26,
                        pinned: false,
                    },
                    &mut quads,
                    &mut Vec::new(),
                );
                assert!(ws.fx.cursor_mut().beamrod.is_active());
            }
            CursorDependent::Comet => {
                ws.fx.cursor_mut().comet.tick(
                    cur,
                    now,
                    1.0,
                    geom,
                    &crate::cursor_comet::CometConfig {
                        enabled: true,
                        intensity: 1.0,
                        color: 0x0044_CCFF,
                        accent: 0x00DD_FFFF,
                        dark_theme: true,
                        theme_fg: 0x00C8_D3F5,
                        theme_bg: 0x001A_1B26,
                        pinned: false,
                    },
                    &mut quads,
                    &mut Vec::new(),
                );
                assert!(ws.fx.cursor_mut().comet.is_active());
            }
            CursorDependent::Phaser => {
                ws.fx.cursor_mut().phaser.tick(
                    cur,
                    now,
                    0.25,
                    1.0,
                    true,
                    geom,
                    &crate::cursor_phaser::PhaserConfig {
                        enabled: true,
                        intensity: 1.0,
                        // This fixture only needs a charged emitter; the base is the
                        // scheduler's business.
                        base: None,
                    },
                    &mut quads,
                );
                assert!(ws.fx.cursor_mut().phaser.is_active());
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
                ws.fx
                    .cursor_mut()
                    .trail
                    .tick(Some((2, 1)), now, &cfg, &mut cells);
                // A deliberate synthetic hop, not a one-glyph echo (which takes the
                // large-delta re-anchor path).
                ws.fx.cursor_mut().trail.note_synthetic_move(now);
                ws.fx.cursor_mut().trail.tick(cur, now, &cfg, &mut cells);
                assert!(ws.fx.cursor_mut().trail.is_active());
            }
            CursorDependent::Cat => {
                ws.fx
                    .companions_mut()
                    .cursor_cat
                    .on_collect(now, aterm_effects::kitty_registry::KittyLook::default());
                assert!(ws.fx.companions_mut().cursor_cat.is_active());
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
        app.capture_words(wid, now);
        assert!(app.windows[&wid].fx.decos().is_active(now));
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
        // The real render tick settles cursor-body engines this config does not enable,
        // so the decoration goes first and the scheduler inputs after, or the rainbow
        // body arrives dormant.
        activate_dependent(app, CursorDependent::WordDecoration, now);
        for effect in CURSOR_DEPENDENTS {
            if matches!(effect, CursorDependent::WordDecoration) {
                continue;
            }
            activate_dependent(app, effect, now);
        }
        let ws = app.windows.get_mut(&crate::WindowId(0)).expect("window 0");
        ws.next_trail_tick = Some(now + Duration::from_millis(16));
        ws.last_trail_fire = Some(now);
        // The chrome-decoration lane is a scheduler input with no focus gate, so arm it
        // here too.
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
        // These latches survive the switch: the canonical-front gate makes them inert.
        assert!(ws.fx.cursor().rainbow.is_active());
        assert!(ws.fx.cursor().droplet.is_active());
        assert!(ws.fx.cursor().beamrod.is_active());
        assert!(ws.fx.cursor().comet.is_active());
        assert!(ws.fx.cursor().phaser.is_active());
        assert!(ws.fx.companions().cursor_cat.is_active());
        assert!(ws.fx.decos().is_active(now));
        assert!(!ws.cursor_fx_active(now, true));
        assert!(!ws.cursor_fx_active(now, false));
        assert!(!ws.terminal_effect_frame_active(now, true));
        assert_eq!(ws.static_cursor_cat_deadline(now, false, false), None);
        // The decoration latch has no focus gate of its own, so the canonical-front
        // boundary alone keeps a decoration from pacing a native tab:
        // `park_terminal_effect_scheduler` drops it; a returning terminal refreshes it on
        // frame one.
        assert!(!ws.deco_anim_frame_active(now));
        assert_eq!(ws.next_trail_tick, None);
        assert_eq!(ws.last_trail_fire, None);
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
                ws.fx.companions_mut().cursor_cat = crate::kitty_cursor::CursorCat::default();
            }
            let dependents = ws.cursor_dependents_need_frame_cadence(now, true);
            assert!(dependents, "{effect:?} must request frame cadence");
            assert_eq!(
                ws.cursor_fx_active(now, true),
                ws.fx.cursor_mut().glow.is_active() || dependents,
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
            ws.fx.cursor().trail.is_active(),
            "ordinary sync must not wipe live effects"
        );
        assert!(ws.cursor_fx_active(now, true));
        assert_eq!(
            ws.next_trail_tick,
            Some(deadline),
            "ordinary terminal bookkeeping preserves the armed cadence"
        );
    }

    /// A headless instance on the Security page runs the park hop and publishes no
    /// posture: no probe read, no `NSWorkspace`, no block to press.
    #[test]
    fn a_headless_security_page_publishes_no_consent_posture_and_touches_no_probe() {
        let mut app = App::headless_for_test();
        assert!(app.headless, "the guard under test is the headless one");
        assert!(
            app.open_settings_tab(crate::native_settings::SettingsRoute::Security),
            "the Security page opens headless like any other route"
        );
        // The per-park hop, run explicitly. It must stay silent.
        app.sync_settings_consent_posture();

        let (_, _, view, _) = app
            .native_settings_view_target()
            .expect("the Security view is live");
        let Some(crate::native_app::AppViewState::Settings(state)) =
            app.native_runtime.view_state(view)
        else {
            unreachable!()
        };
        assert_eq!(
            state.route,
            crate::native_settings::SettingsRoute::Security,
            "the view really is on the page that carries the block"
        );
        assert!(
            state.macos_access_for_test().is_none(),
            "a headless instance publishes no posture, so there is nothing to press"
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

        // Advance the service while no view listens: the reopened view's first edit
        // must base on this revision, not 1.
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
        const ROOT: &str = "ATERM_NATIVE_SETTINGS_TAB_IDENTITY_ROOT";
        if !crate::test_alone::in_child() {
            let root = aterm_tempfile::TempDir::new().unwrap();
            crate::test_alone::run_alone_with(&[
                (ROOT, root.path().as_os_str()),
                ("XDG_CONFIG_HOME", root.path().as_os_str()),
            ]);
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
        // Every route is a page of the one Settings tab, Manual included.
        for route in crate::native_settings::SettingsRoute::ALL {
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

        assert!(
            app.native_runtime.config_editor_document().is_none(),
            "going to the Manual page opens no editor"
        );
        // Open aterm.toml opens the canonical config Editor rather than another
        // settings pane; returning from it must recover the original Settings
        // presentation.
        app.open_config_editor_in_front()
            .expect("Open aterm.toml opens the editor");
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

    /// A finished move reaches the projection: the worker's report arrives, the
    /// wake folds it (and invalidates the census), and the projection carries
    /// the per-copy outcomes and the status row. Publishing it to a view is
    /// `a_published_move_renders_on_the_security_page`.
    #[test]
    fn a_finished_move_reaches_the_projection() {
        let mut app = App::headless_for_test();
        let (tx, rx) = std::sync::mpsc::channel();
        assert!(app.claimant_retire.start(
            "x".to_string(),
            vec![std::path::PathBuf::from("/A/x.app.rollback")],
            move || {
                let _ = tx.send(());
            },
        ));
        rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the worker finished");
        // A second press refused while that move ran is superseded when the move ends:
        // the status is the move's.
        app.claimant_retire_refusal =
            Some("a move is already under way, or its worker could not start");
        let before = app.consent.census_generation_for_test();
        app.on_claimant_retired();
        assert_eq!(
            app.claimant_retire_refusal, None,
            "the move's end supersedes it"
        );
        assert!(
            !app.claimant_retire.take_arrival(),
            "the wake folded the arrival"
        );
        assert_eq!(
            app.consent.census_generation_for_test(),
            before + 1,
            "and invalidated the census, since the disk changed"
        );
        let access = app.macos_access_projection(true);
        assert_eq!(
            access.retired,
            Some(vec![(
                std::path::PathBuf::from("/A/x.app.rollback"),
                aterm_containment::consent::Retired::NotOffered
            )]),
            "the inert census offers nothing, so it stayed"
        );

        app.claimant_retire.set_report_for_test(vec![(
            std::path::PathBuf::from("/A/x.app.rollback"),
            aterm_containment::consent::Retired::Moved,
        )]);
        let moved = app.macos_access_projection(true);
        assert_eq!(
            moved.retired,
            Some(vec![(
                std::path::PathBuf::from("/A/x.app.rollback"),
                aterm_containment::consent::Retired::Moved
            )])
        );
    }

    /// The publish step hands a real projection to a Security view, and the
    /// view renders the move: its status row and its per-copy line.
    #[test]
    fn a_published_move_renders_on_the_security_page() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Security));
        let (_, view) = app.active_native_view(wid).expect("Settings view");
        app.claimant_retire.set_report_for_test(vec![(
            std::path::PathBuf::from("/A/x.app.rollback"),
            aterm_containment::consent::Retired::Moved,
        )]);
        let access = app.macos_access_projection(true);
        app.publish_macos_access(
            &[(wid, view)],
            &access,
            crate::native_settings::ConsentGestures::inert(),
        );
        let Some(crate::native_app::AppViewState::Settings(state)) =
            app.native_runtime.view_state(view)
        else {
            panic!("a Settings view");
        };
        let published = state.macos_access_for_test().expect("published");
        assert_eq!(
            published.retired,
            Some(vec![(
                std::path::PathBuf::from("/A/x.app.rollback"),
                aterm_containment::consent::Retired::Moved
            )]),
            "the view holds the move the projection carried"
        );
    }

    /// The wakes that land a consent fact (a probe answer, a warm-up row, a
    /// *Move to Trash* report) leave the repaint to the park's publish step,
    /// which repaints the window whose Security block changed and no other.
    /// Window 0 shows Security, window 1 a terminal. A changed block clears
    /// window 0's present key and keeps window 1's; the same block published
    /// again clears nothing (the negative control).
    #[test]
    fn a_consent_publication_repaints_only_the_changed_security_view() {
        let mut app = App::headless_for_test();
        let security = crate::WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Security));
        let (_, view) = app.active_native_view(security).expect("Settings view");
        let terminal = crate::WindowId(1);
        app.install_window_state(terminal, crate::stub_session(1), 24, 80);
        let windows = [security, terminal];
        let publish = |app: &mut App, access: &crate::native_settings::MacosAccess| {
            app.publish_macos_access(
                &[(security, view)],
                access,
                crate::native_settings::ConsentGestures::inert(),
            );
        };
        // Every window presented its frame, as the redraw after a present stamps it.
        let present = |app: &mut App| {
            let mut term = aterm_core::terminal::Terminal::new(2, 3);
            let key = crate::early_out_tests::frame_key(&mut term, false, false, None);
            for wid in windows {
                app.windows
                    .get_mut(&wid)
                    .expect("window")
                    .stamp_present_decision(key);
            }
        };
        let presented = |app: &App| windows.map(|wid| app.windows[&wid].last_present.is_some());

        let before = app.macos_access_projection(true);
        publish(&mut app, &before);
        present(&mut app);
        let after = crate::native_settings::MacosAccess {
            fda: if before.fda == aterm_containment::FdaState::Granted {
                aterm_containment::FdaState::Denied
            } else {
                aterm_containment::FdaState::Granted
            },
            ..before.clone()
        };
        publish(&mut app, &after);
        assert_eq!(
            presented(&app),
            [false, true],
            "the changed Security view repaints; the terminal window keeps its frame"
        );
        present(&mut app);
        publish(&mut app, &after);
        assert_eq!(
            presented(&app),
            [true, true],
            "an unchanged block repaints nothing"
        );
    }

    /// The Full Disk Access probe and the DEVELOPER TOOLS card's `log show` start
    /// only while a Security view is on screen (debugger design §9.1, O12). A
    /// Security page in a background tab, or in a window the OS reports occluded,
    /// is served from the two slots and starts nothing, however many parks publish
    /// it; the same view in its window's active tab, in a window not occluded,
    /// dispatches each once (the positive control).
    #[test]
    fn a_hidden_security_page_starts_no_fda_or_devtools_probe() {
        use crate::consent_state::SlotKind;
        use aterm_containment::{DevToolProbe, DevToolProbeLabel, FdaProbe, ProbeGate};
        use std::sync::atomic::{AtomicUsize, Ordering};
        static DEVTOOLS: AtomicUsize = AtomicUsize::new(0);
        static FDA: AtomicUsize = AtomicUsize::new(0);
        fn devtools(_gate: ProbeGate, _subject: &str) -> DevToolProbe {
            DEVTOOLS.fetch_add(1, Ordering::AcqRel);
            DevToolProbe::refused(DevToolProbeLabel::NoRecentRequest)
        }
        fn fda(_gate: ProbeGate) -> FdaProbe {
            FDA.fetch_add(1, Ordering::AcqRel);
            aterm_containment::consent::classify_probe(aterm_containment::ProbeOutcome::Ok)
        }
        fn subject() -> Option<&'static str> {
            Some("com.example.subject")
        }
        let mut app = App::headless_for_test();
        app.consent.set_devtools_arms_for_test(devtools, subject);
        app.consent.set_fda_arm_for_test(fda);
        let (tx, rx) = std::sync::mpsc::channel();
        app.consent
            .set_slot_wake(std::sync::Arc::new(move |kind: SlotKind| {
                let _ = tx.send(kind);
            }));
        let runs = || {
            [
                FDA.load(Ordering::Acquire),
                DEVTOOLS.load(Ordering::Acquire),
            ]
        };
        let before = runs();
        let ran = || {
            let now = runs();
            [now[0] - before[0], now[1] - before[1]]
        };
        let idle = |app: &App| {
            app.consent.fda_slot_idle_for_test() && app.consent.devtools_slot_idle_for_test()
        };
        let wid = crate::WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Security));
        let (settings, _, _, view) = app.settings_tab_in_window(wid).expect("a Settings tab");
        let publish = |app: &mut App| {
            for _ in 0..3 {
                app.publish_security_page(vec![(wid, view)]);
            }
        };

        let other = (0..app.windows.get_mut(&wid).expect("window 0").tab_set.len())
            .find(|&index| index != settings)
            .expect("the window holds another tab");
        assert!(
            app.windows
                .get_mut(&wid)
                .expect("window 0")
                .tab_set
                .switch_to_index(other)
        );
        assert!(!app.settings_view_on_screen(wid, view));
        publish(&mut app);
        assert_eq!(ran(), [0, 0], "a background tab ran one");
        assert!(idle(&app));

        assert!(
            app.windows
                .get_mut(&wid)
                .expect("window 0")
                .tab_set
                .switch_to_index(settings)
        );
        app.windows.get_mut(&wid).expect("window 0").occluded = true;
        assert!(!app.settings_view_on_screen(wid, view));
        publish(&mut app);
        assert_eq!(ran(), [0, 0], "an occluded window ran one");
        assert!(idle(&app));

        // The positive control: on screen, one worker of each for every park.
        app.windows.get_mut(&wid).expect("window 0").occluded = false;
        assert!(app.settings_view_on_screen(wid, view));
        publish(&mut app);
        let mut published = Vec::new();
        while !(published.contains(&SlotKind::Fda) && published.contains(&SlotKind::Devtools)) {
            published.push(
                rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                    .expect("the workers publish"),
            );
        }
        publish(&mut app);
        assert_eq!(ran(), [1, 1]);
        let Some(crate::native_app::AppViewState::Settings(state)) =
            app.native_runtime.view_state(view)
        else {
            panic!("a Settings view");
        };
        let access = state.macos_access_for_test().expect("published");
        assert_eq!(
            (access.fda, access.devtools.probe.label),
            (
                aterm_containment::FdaState::Granted,
                DevToolProbeLabel::NoRecentRequest
            ),
            "the page reads the answers it got"
        );
    }

    /// A confirmed warm-up that the host cannot start says why, in the view it
    /// was confirmed from, in place of the line that said it was asking. A
    /// headless instance starts no worker, so the press is refused at the park.
    #[test]
    fn a_confirmed_warm_up_that_starts_nothing_says_why() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Security));
        let (_, view) = app.active_native_view(wid).expect("Settings view");
        let Some(crate::native_app::AppViewState::Settings(state)) =
            app.native_runtime.view_state_mut(view)
        else {
            panic!("a Settings view");
        };
        state.record_consent_warmup_for_test();
        app.sync_settings_consent_posture();
        let Some(crate::native_app::AppViewState::Settings(state)) =
            app.native_runtime.view_state_mut(view)
        else {
            panic!("a Settings view");
        };
        assert!(!state.take_consent_warmup_request(), "drained");
        let why = crate::consent_warmup::StartOutcome::Refused
            .refusal()
            .expect("a refusal");
        assert!(
            state
                .feedback
                .as_deref()
                .is_some_and(|said| said.contains(why)),
            "the view says why: {:?}",
            state.feedback
        );
    }

    /// A confirmed debugger-access press is drained at the park and handed to
    /// `begin_debugger_access_change`, whose gates refuse it on a headless
    /// instance before any arm is reached; the refusal rides the row's facts,
    /// nothing is in flight, and the park arms no deadline for it.
    #[test]
    fn a_confirmed_debugger_change_on_a_headless_instance_starts_nothing_and_says_why() {
        use aterm_containment::consent::DebuggerChange;
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Security));
        let (_, view) = app.active_native_view(wid).expect("Settings view");
        let Some(crate::native_app::AppViewState::Settings(state)) =
            app.native_runtime.view_state_mut(view)
        else {
            panic!("a Settings view");
        };
        state.record_debugger_change_for_test(DebuggerChange::On);
        app.sync_settings_consent_posture();
        let Some(crate::native_app::AppViewState::Settings(state)) =
            app.native_runtime.view_state_mut(view)
        else {
            panic!("a Settings view");
        };
        assert_eq!(state.take_debugger_change_request(), None, "drained");
        let facts = app.debugger_access_facts();
        assert_eq!(
            facts.refusal,
            Some(crate::debugger_access::Refusal::NoWindow)
        );
        assert_eq!(facts.in_flight, None);
        assert_eq!(
            facts.probe.label,
            aterm_containment::consent::DebuggerProbeLabel::RefusedDisabled,
            "the inert probe arm: nothing was consulted"
        );
        assert!(!facts.offers(DebuggerChange::On) && !facts.offers(DebuggerChange::Off));
        assert_eq!(
            app.begin_debugger_access_change(
                DebuggerChange::Off,
                crate::native_settings::OwnerAnswer::yes_for_test()
            ),
            Err(crate::debugger_access::Refusal::NoWindow)
        );
        app.tick_debugger_access();
        assert!(
            !app.debugger_change.records_asked(),
            "headless: nothing is read"
        );
        assert_eq!(
            app.next_debugger_change_deadline(std::time::Instant::now()),
            None
        );
    }

    /// THE `WF(Notice)` SITE, App level (debugger design §5.1, §5.4): while a
    /// change is under way the App folds the lane's `RESULT_RECHECK` into the
    /// park, and the park's own consent-lane tick takes a result whose wake
    /// never came and re-probes. The headless guard is the lane's `windowed`
    /// flag, so a windowed lane stands in here; nothing spawns a process — the
    /// job is armed and published by hand, and the probe arm is inert.
    // The lane reads the records only on macOS (`App::tick_debugger_access`):
    // DevToolsSecurity's debugger access is macOS's.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_debugger_change_under_way_is_folded_into_the_park_and_read_by_its_tick() {
        use crate::debugger_access::{DebuggerChangeLane, Outcome, RESULT_RECHECK};
        use crate::metrics::DeadlineOwner;
        use aterm_containment::consent::{
            DebuggerChange, DebuggerMember, DebuggerMode, DebuggerProbe, DebuggerProbeLabel,
            DevtoolsChangeRun, DevtoolsChild,
        };
        let fold = |app: &App, now: std::time::Instant| {
            let (mut deadline, mut owner) = (None, DeadlineOwner::None);
            app.fold_consent_lane_deadlines(now, &mut deadline, &mut owner);
            (deadline, owner)
        };
        let mut app = App::headless_for_test();
        app.debugger_change = DebuggerChangeLane::with_liveness(|_| false);
        let now = std::time::Instant::now();
        app.tick_consent_lanes(now);
        assert!(app.debugger_change.records_asked(), "the records are read");
        assert_eq!(fold(&app, now).0, None, "an idle lane arms nothing");
        app.debugger_change.observe(
            DebuggerProbe {
                mode: DebuggerMode::Disabled,
                member: DebuggerMember::Admin,
                label: DebuggerProbeLabel::StatusOk,
            },
            1,
        );
        let job = app
            .debugger_change
            .begin(
                DebuggerChange::On,
                crate::native_settings::OwnerAnswer::yes_for_test(),
            )
            .expect("a confirmed press over the precondition")
            .job();
        let slot = app.debugger_change.slot();
        let child = DevtoolsChild {
            pid: 4242,
            birth: None,
        };
        slot.publish_spawned(job, child);
        app.tick_consent_lanes(now);
        let flight = app.debugger_access_facts().in_flight;
        assert!(flight.is_some(), "the park read the spawned child");
        assert_eq!(
            fold(&app, now),
            (Some(now + RESULT_RECHECK), DeadlineOwner::DebuggerChange),
            "a child in flight keeps the park re-reading"
        );
        // The child exits; its wake is dropped (an uncommitted handoff).
        slot.publish_result(
            job,
            DevtoolsChangeRun::Exited {
                code: Some(0),
                stdout_head: String::new(),
                stderr_head: String::new(),
            },
        );
        app.tick_consent_lanes(now);
        let facts = app.debugger_access_facts();
        assert_eq!(facts.in_flight, None, "the result was taken");
        assert_eq!(
            facts.last_outcome,
            Some(Outcome::Unobserved(DebuggerChange::On)),
            "and re-probed: the inert arm reads nothing, and says so"
        );
        assert_eq!(fold(&app, now).0, None, "ended: nothing left to arm");
    }

    /// THE DEVELOPER TOOLS HAND-OVER at the park (owner ask 2026-10-15), the
    /// debugger change's sibling: the window's own hand-over waits for a quiet
    /// moment and arms nothing while no aterm window is focused; the owner's
    /// press opens the page and shows the helper panel through the one lane;
    /// while the panel is up the App folds its turn into the park as
    /// `DeadlineOwner::DevToolsHandOver`, and the park's own consent-lane tick
    /// reads the list on a worker and takes the result whatever its wake did. A
    /// read that finds aterm on ends the digest line; the panel closes after
    /// its done words and nothing is left to arm. A windowed lane with
    /// stand-in arms stands in on a headless App; nothing reaches the OS.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_devtools_hand_over_under_way_is_folded_into_the_park_and_read_by_its_tick() {
        use crate::devtools_helper::{DONE_FOR, HandOver, Origin, PANEL_TICK, Phase, test_arms};
        use crate::metrics::DeadlineOwner;
        let fold = |app: &App, now: std::time::Instant| {
            let (mut deadline, mut owner) = (None, DeadlineOwner::None);
            app.fold_consent_lane_deadlines(now, &mut deadline, &mut owner);
            (deadline, owner)
        };
        let mut app = App::headless_for_test();
        let now = std::time::Instant::now();
        assert_eq!(
            app.hand_devtools_to_owner(Origin::Digest),
            HandOver::Headless,
            "a headless instance does nothing"
        );
        test_arms::stand_in(&mut app, test_arms::opening());
        assert!(app.devtools_watch.wants_read(now), "the launch read");
        // The launch read found aterm not on the list: this process was checked
        // before the switch, so the switch going on owes macOS one ask.
        let _ = app
            .devtools_watch
            .observe(aterm_containment::DevToolGrant::NotGranted, true);
        app.except(&crate::message_reporters::devtools_entry(
            atpkg::devtools::Step::Add,
        ));
        assert_eq!(fold(&app, now).0, None, "nothing waits");
        assert_eq!(
            app.hand_devtools_to_owner(Origin::Automatic),
            HandOver::Queued,
            "the window's own waits for a quiet moment"
        );
        assert_eq!(
            fold(&app, now).0,
            None,
            "no aterm window focused: the focus gain wakes the loop"
        );
        assert_eq!(app.hand_devtools_to_owner(Origin::Digest), HandOver::Opened);
        assert_eq!(
            (test_arms::opens(), test_arms::opened_notes()),
            (1, 1),
            "the page, and the shared marker"
        );
        assert_eq!(
            app.devtools_helper.queued(),
            None,
            "the press ends the wait"
        );
        assert!(
            app.devtools_watch.wants_read(now),
            "the owner's return reads"
        );
        assert_eq!(
            app.devtools_helper.panel().map(|p| p.phase()),
            Some(Phase::Explain)
        );
        let (deadline, owner) = fold(&app, now);
        assert_eq!(owner, DeadlineOwner::DevToolsHandOver);
        assert!(deadline.is_some_and(|at| at <= now + PANEL_TICK));
        assert_eq!(
            app.devtools_helper.refreshes_started(),
            0,
            "not switched on yet: macOS is asked nothing"
        );
        // The park's turn reads the list on a worker; the result is in the
        // slot whether or not its wake (no event loop here) came.
        let t1 = std::time::Instant::now() + PANEL_TICK;
        let start = std::time::Instant::now();
        while !app.messages.digest().is_empty() {
            assert!(start.elapsed() < Duration::from_secs(60), "hang");
            app.tick_consent_lanes(t1);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            app.devtools_helper.panel().map(|p| p.phase()),
            Some(Phase::Done),
            "on: the done words"
        );
        assert_eq!(
            app.devtools_helper.refreshes_started(),
            1,
            "switched on: macOS is asked about aterm again, once"
        );
        app.tick_consent_lanes(t1 + DONE_FOR);
        assert!(app.devtools_helper.panel().is_none(), "done closes itself");
        assert_eq!(
            fold(&app, t1 + DONE_FOR).0,
            None,
            "ended: nothing left to arm"
        );
        assert_eq!(
            app.hand_devtools_to_owner(Origin::Automatic),
            HandOver::Refused(crate::devtools_helper::Refusal::Granted),
            "the panel's read is the lane's reading now"
        );
    }

    /// THE DEVELOPER TOOLS CARD'S BUTTON (owner ask 2026-10-15) is the one
    /// hand-over: its press is recorded on the view, the park performs it
    /// through `App::hand_devtools_to_owner(Origin::Settings)` — the page, and
    /// the helper panel beside it — and what it did lands on the view's bar
    /// with the route in words. An owner's press: the owner's Not now on record
    /// does not hold it back. A headless instance and a page that did not open
    /// (the negative controls) say that nothing opened and show no panel. Stand-in
    /// arms on a headless App; nothing reaches the OS.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_developer_tools_cards_press_is_the_one_hand_over() {
        use crate::devtools_helper::{Origin, Phase, test_arms};
        use crate::native_settings::{MACOS_ACCESS_OPEN_DEVTOOLS, SettingsRoute};
        let words =
            crate::menu::privacy_settings_path_words(crate::menu::PrivacyPane::DeveloperTools);
        let press_and_park = |app: &mut App| {
            let wid = crate::WindowId(0);
            assert!(app.open_settings_tab(SettingsRoute::Security));
            let (_, view) = app.active_native_view(wid).expect("Settings view");
            // The page as a window publishes it, with arms that may ask the host
            // (a headless App publishes nothing, and an unpublished view asks
            // nothing: `native_settings`' own negative controls).
            let access = app.macos_access_projection(false);
            app.publish_macos_access(
                &[(wid, view)],
                &access,
                crate::native_settings::ConsentGestures::confirming(),
            );
            app.dispatch_native_view_event(
                wid,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new(MACOS_ACCESS_OPEN_DEVTOOLS),
                    value: None,
                }),
            )
            .unwrap();
            app.sync_settings_consent_posture();
            let Some(crate::native_app::AppViewState::Settings(state)) =
                app.native_runtime.view_state_mut(view)
            else {
                panic!("a Settings view");
            };
            assert!(!state.take_devtools_hand_over_request(), "drained");
            state.feedback.clone().expect("the bar says what happened")
        };
        let not_opened = format!("System Settings did not open. The setting is at {words}.");

        let mut headless = App::headless_for_test();
        assert_eq!(press_and_park(&mut headless), not_opened);
        assert!(headless.devtools_helper.panel().is_none());

        let mut app = App::headless_for_test();
        test_arms::stand_in(&mut app, test_arms::declining());
        let opens = test_arms::opens();
        assert_eq!(
            press_and_park(&mut app),
            format!("Opened System Settings \u{2014} look under {words}.")
        );
        assert_eq!(
            test_arms::opens(),
            opens + 1,
            "one page, whatever was declined"
        );
        let panel = app.devtools_helper.panel().expect("the helper panel");
        assert_eq!(
            (panel.origin(), panel.phase()),
            (Origin::Settings, Phase::Explain)
        );
        assert!(
            app.devtools_watch.wants_read(std::time::Instant::now()),
            "the owner's return reads the list"
        );

        let mut refused = App::headless_for_test();
        test_arms::stand_in(&mut refused, test_arms::refusing());
        let opens = test_arms::opens();
        assert_eq!(press_and_park(&mut refused), not_opened);
        assert_eq!(test_arms::opens(), opens + 1, "it was asked");
        assert!(refused.devtools_helper.panel().is_none());
    }

    /// A PRESS ON A LIST READ EMPTY waits for `spctl developer-mode
    /// enable-terminal` before the page can open: the card's bar says the page is
    /// on its way, and when the run ends it says how it came out — opened, opened
    /// with macOS showing no list because the run failed (and the panel says why
    /// and offers Show it), or (the negative control) not — instead of a promise
    /// left standing. Stand-in arms on a headless App; nothing reaches the OS.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_cards_press_that_waits_for_spctl_hears_how_the_page_came_out() {
        use crate::devtools_helper::test_arms;
        use crate::native_settings::{MACOS_ACCESS_OPEN_DEVTOOLS, SettingsRoute};
        let words =
            crate::menu::privacy_settings_path_words(crate::menu::PrivacyPane::DeveloperTools);
        for (reach, came_out) in [
            (
                test_arms::opening(),
                format!("Opened System Settings \u{2014} look under {words}."),
            ),
            (
                test_arms::spctl_failing(),
                format!(
                    "Opened System Settings, but macOS did not show the Developer Tools list \
                     (spctl exited 1). The setting is at {words}."
                ),
            ),
            (
                test_arms::refusing(),
                format!("System Settings did not open. The setting is at {words}."),
            ),
        ] {
            let mut app = App::headless_for_test();
            test_arms::stand_in(&mut app, reach);
            app.devtools_helper.note_reading(
                test_arms::reading(
                    aterm_containment::DevToolGrant::NotGranted,
                    test_arms::absent(0),
                ),
                std::time::Instant::now(),
            );
            let wid = crate::WindowId(0);
            assert!(app.open_settings_tab(SettingsRoute::Security));
            let (_, view) = app.active_native_view(wid).expect("Settings view");
            let access = app.macos_access_projection(false);
            app.publish_macos_access(
                &[(wid, view)],
                &access,
                crate::native_settings::ConsentGestures::confirming(),
            );
            app.dispatch_native_view_event(
                wid,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new(MACOS_ACCESS_OPEN_DEVTOOLS),
                    value: None,
                }),
            )
            .unwrap();
            app.sync_settings_consent_posture();
            let feedback = |app: &mut App| {
                let Some(crate::native_app::AppViewState::Settings(state)) =
                    app.native_runtime.view_state_mut(view)
                else {
                    panic!("a Settings view");
                };
                state.feedback.clone().expect("the bar says what happened")
            };
            assert!(
                feedback(&mut app).starts_with("Asking macOS to show the Developer Tools list"),
                "{}",
                feedback(&mut app)
            );
            // The run ends; the park's turn opens the page and tells the bar.
            let start = std::time::Instant::now();
            while app.devtools_helper.listing().is_some() {
                assert!(start.elapsed() < std::time::Duration::from_secs(60), "hang");
                app.tick_devtools_hand_over(std::time::Instant::now());
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            assert_eq!(feedback(&mut app), came_out);
            // The panel beside the page says what the run said.
            if let Some(panel) = app.devtools_helper.panel() {
                let content = panel.content();
                let failed = content
                    .texts()
                    .any(|(text, _)| text.contains("macOS did not show the list (spctl exited 1)"));
                assert_eq!(failed, came_out.contains("did not show"), "{content:?}");
                assert_eq!(
                    content
                        .buttons()
                        .contains(&crate::devtools_helper::Button::ShowList),
                    failed,
                    "Show it again only after a failed run: {content:?}"
                );
            }
        }
    }

    /// A HAND-OVER THE OWNER DID NOT PRESS, JUDGED QUIET BEFORE ITS `spctl` RUN, IS
    /// JUDGED AGAIN WHEN THE RUN ENDS (the run takes as long as syspolicyd takes): the
    /// moment has passed — here no aterm window is focused — so System Settings does
    /// not take focus; the hand-over waits for the next quiet moment with the run's
    /// answer, and then opens the page without running `spctl` again, the panel
    /// saying Terminal is aterm's doing. A reading that saw aterm on while the run was
    /// out drops it. NEGATIVE CONTROL: the owner's own press opens when its run ends,
    /// however unquiet the moment. Stand-in arms on a headless App; nothing reaches
    /// the OS.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_hand_over_judged_quiet_before_spctl_is_judged_again_when_it_ends() {
        use crate::devtools_helper::{HandOver, Moment, Origin, test_arms};
        use aterm_containment::DevToolGrant;
        let run_out = |app: &mut App| {
            let start = std::time::Instant::now();
            while app.devtools_helper.listing().is_some() {
                assert!(start.elapsed() < std::time::Duration::from_secs(60), "hang");
                app.tick_devtools_hand_over(std::time::Instant::now());
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        };
        let empty = || test_arms::reading(DevToolGrant::NotGranted, test_arms::absent(0));
        let mut app = App::headless_for_test();
        test_arms::stand_in(&mut app, test_arms::opening());
        app.devtools_helper
            .note_reading(empty(), std::time::Instant::now());
        let opens = test_arms::opens();
        assert_eq!(
            app.devtools_hand_over(Origin::Automatic, Moment::Quiet),
            HandOver::Listing
        );
        run_out(&mut app);
        assert_eq!(
            test_arms::opens(),
            opens,
            "the moment passed: System Settings does not take focus"
        );
        assert!(app.devtools_helper.panel().is_none());
        assert_eq!(
            app.devtools_helper.queued(),
            Some(Origin::Automatic),
            "held for the next quiet moment"
        );
        let (origin, listed) = app
            .devtools_helper
            .take_queued(std::time::Instant::now(), true)
            .expect("held");
        assert_eq!(listed, Some(Ok(())), "with the run's answer");
        assert_eq!(
            app.devtools_hand_over_carrying(origin, Moment::Quiet, listed),
            HandOver::Opened
        );
        assert!(app.devtools_helper.listing().is_none(), "no second run");
        assert_eq!(test_arms::opens(), opens + 1);
        let panel = app.devtools_helper.panel().expect("the helper panel");
        assert!(
            panel
                .content()
                .texts()
                .any(|(text, _)| text == atpkg::devtools::only_aterm(true))
        );

        // A reading that sees aterm on while the run is out: dropped at its end.
        let mut granted = App::headless_for_test();
        test_arms::stand_in(&mut granted, test_arms::opening());
        granted
            .devtools_helper
            .note_reading(empty(), std::time::Instant::now());
        assert_eq!(
            granted.devtools_hand_over(Origin::Ctl { now: false }, Moment::Quiet),
            HandOver::Listing
        );
        granted.devtools_helper.note_reading(
            test_arms::reading(DevToolGrant::Granted, test_arms::on()),
            std::time::Instant::now(),
        );
        let opens = test_arms::opens();
        run_out(&mut granted);
        assert_eq!(test_arms::opens(), opens, "nothing opens");
        assert_eq!(granted.devtools_helper.queued(), None, "refused: dropped");

        // NEGATIVE CONTROL: the owner's own press opens when its run ends.
        let mut pressed = App::headless_for_test();
        test_arms::stand_in(&mut pressed, test_arms::opening());
        pressed
            .devtools_helper
            .note_reading(empty(), std::time::Instant::now());
        assert_eq!(
            pressed.hand_devtools_to_owner(Origin::Digest),
            HandOver::Listing
        );
        let opens = test_arms::opens();
        run_out(&mut pressed);
        assert_eq!(test_arms::opens(), opens + 1, "the owner asked");
        assert!(pressed.devtools_helper.panel().is_some());
    }

    /// THE WINDOW'S OWN HAND-OVER, REFUSED AS UNKNOWN AT ITS QUIET MOMENT (a later
    /// reading read nothing: no Full Disk Access, no new program in tccd's window), is
    /// no decision against it: the next `not_granted` reading asks for it again,
    /// rather than the once-per-process latch keeping it from ever coming back in this
    /// process. Nothing opens on `unknown`. NEGATIVE CONTROL: a hand-over refused for
    /// the owner's Not now keeps the latch. Stand-in arms on a headless App.
    #[cfg(target_os = "macos")]
    #[test]
    fn an_automatic_hand_over_refused_as_unknown_asks_again_at_the_next_reading() {
        use crate::devtools_helper::{HandOver, Moment, Origin, Reading, Refusal, test_arms};
        use crate::devtools_watch::Step;
        use aterm_containment::DevToolGrant;
        for (reach, reading, why, asks_again) in [
            (
                test_arms::opening(),
                Reading::UNREAD,
                Refusal::Unknown,
                true,
            ),
            (
                test_arms::declining(),
                test_arms::reading(DevToolGrant::NotGranted, test_arms::absent(2)),
                Refusal::Declined,
                false,
            ),
        ] {
            let mut app = App::headless_for_test();
            test_arms::stand_in(&mut app, reach);
            assert_eq!(
                app.devtools_watch.observe(DevToolGrant::NotGranted, false),
                Step::Raise { hand_over: true },
                "the first reading asks"
            );
            app.devtools_helper
                .note_reading(reading, std::time::Instant::now());
            let opens = test_arms::opens();
            assert_eq!(
                app.devtools_hand_over(Origin::Automatic, Moment::Quiet),
                HandOver::Refused(why)
            );
            assert_eq!(test_arms::opens(), opens, "nothing opens");
            assert_eq!(
                app.devtools_watch.observe(DevToolGrant::NotGranted, false),
                Step::Raise {
                    hand_over: asks_again
                },
                "{why:?}"
            );
        }
    }

    /// What the DEVELOPER TOOLS card is told: while the window's own re-read is
    /// out, the switch the hand-over lane last read stands in (no flicker to
    /// "Checking…" on every focus gain); and when aterm last opened the page is
    /// read from the shared marker ONCE — not per park — taken from the lane
    /// when the lane writes it, and read again only after a focus gain.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_devtools_card_reads_the_lanes_switch_and_the_opened_marker_once() {
        use crate::devtools_helper::{Origin, test_arms};
        use std::cell::Cell;
        thread_local! {
            static READS: Cell<u32> = const { Cell::new(0) };
        }
        let reach = test_arms::marked(|| {
            READS.with(|reads| reads.set(reads.get() + 1));
            let now = aterm_time::unix_secs().unwrap_or(0);
            Some(("0.0.0".to_string(), now.saturating_sub(5 * 60)))
        });
        let mut app = App::headless_for_test();
        test_arms::stand_in(&mut app, reach);
        let facts = app.macos_access_projection(false).devtools;
        assert!(facts.probe.label == aterm_containment::DevToolProbeLabel::Pending);
        assert_eq!(
            (facts.list, facts.grant),
            (
                test_arms::absent(2),
                aterm_containment::DevToolGrant::NotGranted
            ),
            "the lane's last read switch stands in while the row is out"
        );
        assert!(
            facts
                .opened_minutes
                .is_some_and(|minutes| (4..=6).contains(&minutes)),
            "{:?}",
            facts.opened_minutes
        );
        for _ in 0..3 {
            let _ = app.macos_access_projection(false);
        }
        assert_eq!(
            READS.with(Cell::get),
            1,
            "the marker is read once, not per park"
        );

        assert!(app.hand_devtools_to_owner(Origin::Digest).brings_owner());
        assert_eq!(
            app.macos_access_projection(false).devtools.opened_minutes,
            Some(0),
            "the lane's own write, without reading the file"
        );
        assert_eq!(READS.with(Cell::get), 1);

        app.devtools_helper.forget_opened();
        let again = app.macos_access_projection(false).devtools.opened_minutes;
        assert!(again.is_some_and(|minutes| minutes >= 4), "{again:?}");
        assert_eq!(
            READS.with(Cell::get),
            2,
            "a focus gain reads it again, once"
        );
    }

    /// `aterm ctl devtools hand-over [now]` (owner ask 2026-10-15) is the one
    /// hand-over with `Origin::Ctl`, held to the window's own rules because
    /// every in-session client may send it: the bare form never opens the page
    /// as it asks but waits for a quiet moment; `now` (a person at `aterm
    /// doctor`'s terminal) shows only the panel — the asker opens the page —
    /// and ends any hand-over still waiting, so the page is not brought up a
    /// second time later. Both answer the owner's Not now, an unread list and
    /// a granted one with nothing; an unstable build holds back only the bare
    /// form; the opened marker holds each to its own window (ten minutes, a
    /// minute for `now`), and `now` is held besides to the window's own minute
    /// since it last answered a `now` — the asker opens the page, and only an
    /// opening moves the marker, so a program that asks and never opens cannot
    /// raise the panel again and again. A headless instance does nothing (the
    /// negative control). Stand-in arms on a headless App; nothing reaches the
    /// OS.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_control_verbs_ask_is_held_to_the_windows_own_rules() {
        use crate::devtools_helper::{
            DevToolsHelperArm, HandOver, Lane, Origin, Phase, Reading, Refusal, test_arms,
        };
        use aterm_containment::DevToolGrant;
        const ASK: Origin = Origin::Ctl { now: false };
        const NOW: Origin = Origin::Ctl { now: true };
        let ask = |app: &mut App, origin| app.hand_devtools_to_owner(origin);

        let mut headless = App::headless_for_test();
        assert_eq!(ask(&mut headless, NOW), HandOver::Headless);

        let mut app = App::headless_for_test();
        test_arms::stand_in(&mut app, test_arms::opening());
        let opens = test_arms::opens();
        let t = std::time::Instant::now();
        assert!(app.devtools_watch.wants_read(t), "the launch read");
        assert_eq!(
            ask(&mut app, ASK),
            HandOver::Queued,
            "the bare form waits for a quiet moment"
        );
        assert_eq!(app.devtools_helper.queued(), Some(ASK));
        assert!(app.devtools_helper.panel().is_none(), "nothing shown yet");
        assert!(
            !app.devtools_watch.wants_read(t),
            "nothing has brought the owner anywhere yet"
        );
        assert_eq!(ask(&mut app, NOW), HandOver::Shown);
        assert_eq!(
            test_arms::opens(),
            opens,
            "`now` leaves the page to its asker"
        );
        let panel = app.devtools_helper.panel().expect("the helper panel");
        assert_eq!((panel.origin(), panel.phase()), (NOW, Phase::Explain));
        assert_eq!(
            app.devtools_helper.queued(),
            None,
            "the asker opens the page: nothing waits to bring it up again"
        );
        assert!(
            app.devtools_watch.wants_read(t),
            "the owner's return reads the list"
        );
        // Nobody opened the page, so the shared marker never moved: the window
        // holds `now` to its minute itself. The bare form is not held by it.
        assert_eq!(
            ask(&mut app, NOW),
            HandOver::Refused(Refusal::Recent),
            "a second `now` inside the minute raises no second panel"
        );
        assert_eq!(ask(&mut app, ASK), HandOver::Queued);
        let minute = std::time::Duration::from_secs(atpkg::devtools::REOPEN_NOW_AFTER_S);
        let at = std::time::Instant::now();
        app.devtools_helper.noted_now_answered(at);
        assert!(
            app.devtools_helper
                .now_answered_within_its_minute(at + minute - std::time::Duration::from_secs(1))
        );
        assert!(
            !app.devtools_helper
                .now_answered_within_its_minute(at + minute),
            "the minute ends"
        );

        // Nothing done, and why: the same answers for both forms.
        let refusals = [
            (test_arms::declining(), None, Refusal::Declined),
            (
                test_arms::opening(),
                Some(Reading::UNREAD),
                Refusal::Unknown,
            ),
            (
                test_arms::opening(),
                Some(test_arms::reading(DevToolGrant::Granted, test_arms::on())),
                Refusal::Granted,
            ),
        ];
        for (reach, reading, why) in refusals {
            let mut app = App::headless_for_test();
            test_arms::stand_in(&mut app, reach);
            if let Some(reading) = reading {
                app.devtools_helper
                    .note_reading(reading, std::time::Instant::now());
            }
            for origin in [ASK, NOW] {
                assert_eq!(ask(&mut app, origin), HandOver::Refused(why), "{origin:?}");
            }
            assert!(app.devtools_helper.panel().is_none(), "{why:?}");
            assert_eq!(app.devtools_helper.queued(), None, "{why:?}");
        }

        // An ad hoc or dev build: only the bare form is held back.
        let mut unstable = App::headless_for_test();
        test_arms::stand_in(&mut unstable, test_arms::unstable());
        assert_eq!(
            ask(&mut unstable, ASK),
            HandOver::Refused(Refusal::Unstable)
        );
        assert_eq!(ask(&mut unstable, NOW), HandOver::Shown);

        // Opened 90 s ago: past `now`'s minute, inside the bare form's ten.
        let mut recent = App::headless_for_test();
        test_arms::stand_in(
            &mut recent,
            test_arms::opened(|window| (90 < window).then_some(90)),
        );
        assert_eq!(ask(&mut recent, ASK), HandOver::Refused(Refusal::Recent));
        assert_eq!(ask(&mut recent, NOW), HandOver::Shown);

        // No panel could be shown: the asker opens the page without it, so
        // the waiting hand-over ends the same way.
        let mut unshown = App::headless_for_test();
        test_arms::stand_in(&mut unshown, test_arms::opening());
        unshown.devtools_helper =
            Lane::windowed_for_test(test_arms::opening(), DevToolsHelperArm::inert());
        unshown.devtools_helper.note_reading(
            test_arms::reading(DevToolGrant::NotGranted, test_arms::absent(2)),
            std::time::Instant::now(),
        );
        assert_eq!(ask(&mut unshown, ASK), HandOver::Queued);
        assert_eq!(ask(&mut unshown, NOW), HandOver::Unshown);
        assert_eq!(unshown.devtools_helper.queued(), None);
    }

    /// What a lane step asks of the host: an unchanged re-probe is the owner's decline
    /// (the shared quiet doubles, nothing retries), and an observed transition is a
    /// record under `privacy.debugger`, never a row.
    #[test]
    fn a_debugger_change_that_reads_unchanged_is_a_decline_and_an_observed_one_a_record() {
        use crate::debugger_access::{DebuggerRecord, DriftStep, LaneStep, Outcome};
        use aterm_containment::consent::DebuggerChange;
        let mut app = App::headless_for_test();
        assert!(
            !app.owner_quiet
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .is_quiet_now_for_test()
        );
        app.apply_debugger_step(LaneStep {
            outcome: Some(Outcome::Changed(DebuggerChange::On)),
            drift: DriftStep {
                record: Some(DebuggerRecord::On),
                write: None,
            },
            ..LaneStep::default()
        });
        assert!(
            !app.owner_quiet
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .is_quiet_now_for_test(),
            "a change that landed is no decline"
        );
        let record = app
            .messages
            .log()
            .records()
            .next_back()
            .expect("the change is on record");
        assert_eq!(
            record.key.as_deref(),
            Some(crate::message_reporters::KEY_DEBUGGER_ACCESS)
        );
        assert!(
            app.messages
                .live_by_key(crate::message_reporters::KEY_DEBUGGER_ACCESS)
                .is_none(),
            "a record never takes the glass"
        );
        app.apply_debugger_step(LaneStep {
            outcome: Some(Outcome::NotChanged(DebuggerChange::On)),
            ..LaneStep::default()
        });
        assert!(
            app.owner_quiet
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .is_quiet_now_for_test(),
            "Cancel in macOS's dialog quiets the page like Cancel in aterm's"
        );
    }

    /// The real park step drains a confirmed press from every Settings view in a
    /// window, one that has left the Security page included, and hands every view the
    /// process's one record of the owner's declines.
    #[test]
    fn the_park_drains_every_settings_view_and_shares_the_decline_record() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Security));
        let (instance, first) = app.active_native_view(wid).expect("Settings view");
        let second = app
            .split_active_with_native(
                wid,
                crate::tab_model::SplitAxis::Horizontal,
                instance,
                crate::native_app::AppViewState::Settings(Box::new(
                    crate::native_settings::SettingsViewState::new(&app.config),
                )),
            )
            .expect("second Settings presentation");
        let settings_tab = app.windows[&wid].tab_set.active_id().unwrap();
        let settings_index = app.windows[&wid]
            .tab_set
            .tabs()
            .iter()
            .position(|tab| tab.id == settings_tab)
            .unwrap();
        // The park discovers background Settings too, in the same stable
        // split order, without materializing the surrounding terminal leaves.
        for _ in 0..32 {
            app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
        }
        assert!(app.active_native_view(wid).is_none());
        assert_eq!(
            app.settings_tabs_in_window(wid),
            [
                (settings_index, settings_tab, instance, first),
                (settings_index, settings_tab, instance, second),
            ]
        );
        for (view, route) in [
            (first, crate::native_settings::SettingsRoute::Security),
            (second, crate::native_settings::SettingsRoute::Appearance),
        ] {
            let Some(crate::native_app::AppViewState::Settings(state)) =
                app.native_runtime.view_state_mut(view)
            else {
                panic!("a Settings view");
            };
            state.record_claimant_retire_for_test(vec![std::path::PathBuf::from(
                "/A/x.app.rollback",
            )]);
            state.navigate(route);
        }
        assert_eq!(app.settings_tabs_in_window(wid).len(), 2);
        app.owner_quiet
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .confirm_both_for_test();
        app.sync_settings_consent_posture();
        assert!(
            !app.owner_quiet
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .has_confirmed_for_test(),
            "the park settles every yes it takes up"
        );
        for view in [first, second] {
            let Some(crate::native_app::AppViewState::Settings(state)) =
                app.native_runtime.view_state_mut(view)
            else {
                panic!("a Settings view");
            };
            assert_eq!(state.take_claimant_retire_request(), None, "drained");
            assert!(
                std::sync::Arc::ptr_eq(state.owner_quiet_for_test(), &app.owner_quiet),
                "every view holds the process's record"
            );
        }
        assert!(
            app.claimant_retire_refusal.is_some(),
            "drained, then refused here because a headless instance starts no worker"
        );
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

        // Close the original terminal tab to reach the native-only shape produced by
        // recursive restore/detach. `false`: it was not the window's final tab.
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
        assert!(
            app.windows[&wid].pending_close_attribution.is_some(),
            "the deferred close stashed its attribution for the teardown"
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
        assert!(
            app.windows[&wid].pending_close_attribution.is_none(),
            "the cancelled teardown never runs, so its attribution must not \
             survive to be journalled on this window's NEXT close"
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

    // -- the typing-sound audition ----------------------------------------

    /// The captured cues as `(voice, gain)`, each asserted to be one plain keystroke.
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

    /// The audition is gated like a keystroke: the "Music effects" master off, zero
    /// volume, or serious mode pushes nothing (and the latch still moves).
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

    /// The reload path auditions a changed voice once and an unchanged one never:
    /// re-applying the same config (or an alias) is silent.
    #[test]
    fn a_config_swap_auditions_only_a_changed_voice_and_never_twice() {
        let mut app = app_with_capture();
        let mut next = app.config.clone();
        // Startup voice (auto) → an unchanged reload: silent.
        assert_eq!(app.typing_sound_to_audition_on_swap(&next), None);
        // A hand edit or native pick that changes the voice: one audition.
        next.trail_sound_style = Some("droplet".into());
        assert_eq!(
            app.typing_sound_to_audition_on_swap(&next),
            Some(SoundVoice::Of(GlowStyle::Water))
        );
        // …and the swap latches it, so re-applying the same config is silent.
        app.audition_typing_sound(SoundVoice::Of(GlowStyle::Water));
        assert_eq!(captured_typed(&mut app).len(), 1);
        assert_eq!(app.typing_sound_to_audition_on_swap(&next), None);
        // Aliases dedupe by voice, not spelling: `water` is still droplet.
        next.trail_sound_style = Some(" Water ".into());
        assert_eq!(app.typing_sound_to_audition_on_swap(&next), None);
        // Clearing the key back to auto from a file edit auditions auto once.
        next.trail_sound_style = None;
        assert_eq!(
            app.typing_sound_to_audition_on_swap(&next),
            Some(SoundVoice::Style)
        );
    }

    /// The window's `a11y_active` latch gates tree building: down until an OS client
    /// attaches (every OS window has an adapter), up on attach, down on detach.
    #[cfg(a11y_tree)]
    #[test]
    fn accessibility_publish_latches_on_attach_and_clears_on_detach() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        let winit_id = aterm_window::window::WindowId::from(1u64);
        app.winit_to_window.insert(winit_id, wid);

        assert!(
            !app.windows[&wid].a11y_active,
            "no OS a11y client has attached, so presents must not build a tree"
        );
        app.on_accessibility_event(crate::a11y_adapter::Event {
            window_id: winit_id,
            window_event: crate::a11y_adapter::WindowEvent::InitialTreeRequested,
        });
        assert!(
            app.windows[&wid].a11y_active,
            "InitialTreeRequested is the attach edge — publishing must resume"
        );
        app.on_accessibility_event(crate::a11y_adapter::Event {
            window_id: winit_id,
            window_event: crate::a11y_adapter::WindowEvent::AccessibilityDeactivated,
        });
        assert!(
            !app.windows[&wid].a11y_active,
            "the client detached; a re-attach re-fires InitialTreeRequested"
        );
    }

    #[cfg(a11y_tree)]
    #[test]
    fn split_accessibility_consumes_the_presented_plan_not_live_layout() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        app.split_active_stub_tab(wid);
        let (rows, cols) = {
            let window = &app.windows[&wid];
            (usize::from(window.rows), usize::from(window.cols))
        };
        assert!(
            app.redraw_compose(wid, rows, cols, false, false, None, 0, Instant::now())
                .is_some(),
            "the two-pane fixture materializes a composed frame"
        );
        let presented = app
            .active_visible_leaf_plan(wid)
            .expect("presented canonical plan");
        assert_eq!(presented.leaves.len(), 2);
        let presented_focus = presented.focused;
        let presented_focus_leaf = presented
            .leaf(presented_focus)
            .expect("presented focused leaf")
            .clone();

        // Negative controls: advance the live pane count and the focused leaf's
        // geometry without composing a frame. Re-resolving the tree from live state
        // would describe three panes over the presented two-pane snapshot.
        app.split_active_stub_tab_dir(wid, crate::pane::SplitDir::Horizontal);
        let live = app
            .active_visible_leaf_plan(wid)
            .expect("new live canonical plan");
        assert_eq!(live.leaves.len(), 3, "live cardinality really advanced");
        assert_ne!(
            live.leaf(presented_focus)
                .expect("the formerly focused leaf survives")
                .rect,
            presented_focus_leaf.rect,
            "live geometry really diverged from the frame on glass"
        );

        let panes = app.grid_a11y_panes(wid, &presented);
        assert_eq!(
            panes.len(),
            presented.leaves.len(),
            "accessibility keeps the presented two-pane cardinality"
        );
        let slot = presented
            .leaves
            .iter()
            .position(|leaf| leaf.view == presented_focus)
            .unwrap();
        let pane = &panes[slot];
        assert_eq!(
            (pane.row_off, pane.col_off, pane.snap.rows, pane.snap.cols),
            (
                presented_focus_leaf.rect.origin.y.round().max(0.0) as usize,
                presented_focus_leaf.rect.origin.x.round().max(0.0) as usize,
                (presented_focus_leaf.rect.size.height.round() as usize).max(1),
                (presented_focus_leaf.rect.size.width.round() as usize).max(1),
            ),
            "pane geometry comes from the exact successful-present plan"
        );
        assert!(
            pane.focused,
            "focus is likewise the presented frame's focus, not the newer live tree's"
        );
    }

    /// Even a stale or unsupported a11y request retires an older cursor-move licence.
    #[cfg(a11y_tree)]
    #[test]
    fn accessibility_action_supersedes_pending_cursor_candidate() {
        use aterm_a11y::model::{Action, ActionRequest, NodeId, TreeId};

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
            ws.fx
                .cursor_mut()
                .glow
                .tick(Some((0, 0)), now, &glow_cfg, geom, &mut glow);
            ws.fx
                .cursor_mut()
                .trail
                .tick(Some((0, 0)), now, &trail_cfg, &mut trail);
            ws.fx.cursor_mut().glow.note_synthetic_move(now);
            ws.fx.cursor_mut().trail.note_synthetic_move(now);
            // The synthetic note licenses the move it is about to make (the engines'
            // own predicate).
            assert!(ws.fx.cursor_mut().glow.move_licensed(now));
            assert!(ws.fx.cursor_mut().trail.move_licensed(now));
        }

        let winit_id = aterm_window::window::WindowId::from(17u64);
        app.winit_to_window.insert(winit_id, wid);
        app.on_accessibility_event(crate::a11y_adapter::Event {
            window_id: winit_id,
            window_event: crate::a11y_adapter::WindowEvent::ActionRequested(ActionRequest {
                context: None,
                action: Action::Focus,
                target_tree: TreeId::ROOT,
                target_node: NodeId(999_999),
                data: None,
            }),
        });
        let ws = app.windows.get_mut(&wid).unwrap();
        // …and an a11y action that targets no live node revokes that licence,
        // so the move it would have decorated draws nothing.
        assert!(!ws.fx.cursor_mut().glow.move_licensed(now));
        assert!(!ws.fx.cursor_mut().trail.move_licensed(now));
        let moved = now + Duration::from_millis(1);
        assert_eq!(
            ws.fx
                .cursor_mut()
                .glow
                .tick(Some((0, 1)), moved, &glow_cfg, geom, &mut glow),
            0
        );
        assert_eq!(
            ws.fx
                .cursor_mut()
                .trail
                .tick(Some((0, 1)), moved, &trail_cfg, &mut trail),
            0
        );
        assert!(glow.is_empty() && trail.is_empty());
    }
}

/// The production wiring for chrome-message live regions: the `App` methods that read
/// the live window and decide what a screen reader is told (which band is on the
/// glass, how the frame cut it), on a real `App`. The pure `a11y_tree` layer is
/// tested in its own module.
#[cfg(all(test, a11y_tree))]
mod a11y_message_wiring_tests {
    use std::time::Instant;

    use crate::a11y_tree::{ChromeMessage, GridMessage};
    use crate::{App, WindowId};

    fn message(app: &App, wid: WindowId, kind: ChromeMessage) -> GridMessage {
        app.grid_a11y_messages(wid)
            .into_iter()
            .find(|m| m.message == kind)
            .unwrap_or_else(|| panic!("{kind:?} is not published"))
    }

    /// A band row is announced where it is painted, and only while it is: the tree reads
    /// the committed `message_band_rows` the splice trims to, and volatile figures stay
    /// out of the spoken half so a byte counter never re-announces the row.
    #[test]
    fn a_band_row_is_announced_at_the_row_it_reserves_and_only_while_it_holds_it() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.prepare_terminal_capture_grid(wid).unwrap();
        let downloading = |done: u64| aterm_update::Progress::Downloading {
            version: "0.64.0".to_string(),
            bytes_done: done * 1024 * 1024,
            bytes_total: 480 * 1024 * 1024,
        };
        // A person asked (Check for Updates): the download is their wait and takes the
        // row.
        app.note_update_check_asked(true);
        app.note_update_progress(&downloading(120));
        assert_eq!(
            app.message_band_rows, 0,
            "inside its grace the row is not on the glass (design §10.4.3)"
        );
        assert!(app.grid_a11y_messages(wid).is_empty(), "…nor in the tree");
        app.settle_messages(Instant::now() + aterm_messages::PROGRESS_GRACE * 2);
        assert_eq!(app.message_band_rows, 1, "the update lane reserved its row");

        let band = message(&app, wid, ChromeMessage::BandRow0);
        assert_eq!(band.bar_row, Some(0), "the only row is the top row");
        assert_eq!(band.progress, Some(0.25));
        assert!(!band.busy, "a download has a fraction");
        assert!(band.activates, "a press opens Software Update");
        let figures = band.detail.clone().expect("the byte counter is described");
        assert!(
            figures.contains("126 MB"),
            "the stats ride the description: {figures:?}"
        );
        assert!(
            !band.text.contains("126 MB"),
            "the volatile figures must not ride the announced sentence: {:?}",
            band.text
        );
        assert_eq!(
            band.capsules.len(),
            1,
            "a downloading row carries only the implicit Details capsule"
        );

        // Further bytes move the description and leave the announced sentence alone (a
        // byte tick restates the live row, never a new one).
        app.note_update_progress(&downloading(400));
        let later = message(&app, wid, ChromeMessage::BandRow0);
        assert_eq!(later.text, band.text, "no announcement for a byte count");
        assert_ne!(later.detail, band.detail);

        // A row the frame has not committed is not described at all — the same
        // trim `splice_message_band` applies to its painted rows.
        app.message_band_rows = 0;
        assert!(
            app.grid_a11y_messages(wid).is_empty(),
            "a row with no committed slot is not on the glass"
        );
    }

    /// A row that asks is an alert, not status: the Full Disk Access question reaches
    /// assistive technology with the alert role at the band's polite politeness, while
    /// work in flight stays a progress indicator.
    #[test]
    fn a_decision_row_is_an_alert() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.prepare_terminal_capture_grid(wid).unwrap();
        app.post_message(crate::message_reporters::file_access_question(
            &crate::consent_card::NeedWords::sample(),
        ));
        let ask = message(&app, wid, ChromeMessage::BandRow0);
        assert_eq!(ask.text, crate::consent_card::FDA_TITLE);
        assert!(ask.alarm, "a question is an alert");
        assert!(ask.progress.is_none() && !ask.busy);
        let mut app = App::headless_for_test();
        app.prepare_terminal_capture_grid(wid).unwrap();
        app.announce_toolchain_pass("installing 10 ALab program(s)", true);
        let work = message(&app, wid, ChromeMessage::BandRow0);
        assert!(!work.alarm, "work in flight is not an alert");
        assert!(work.busy);
    }

    /// Activating a band row performs its body press (the row's `Details ›`, Settings ▸
    /// Messages at that entry in this window); the `Details ›` child carries its full
    /// label and does the same; an authored child performs its own intent; an activate
    /// on a row the frame no longer carries does nothing.
    #[test]
    fn activating_a_band_row_performs_its_body_press_and_a_capsule_child_its_intent() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.prepare_terminal_capture_grid(wid).unwrap();
        let id = app.post_message(
            aterm_messages::Message::new(
                aterm_messages::tags::TOOLCHAIN,
                aterm_messages::Severity::Info,
                "Installing ALab toolchain",
            )
            .line("trust — extracting")
            .action(aterm_messages::Intent::OpenSettings {
                route: "/packages".into(),
            })
            .hold(aterm_messages::Hold::Live {
                stale_after: aterm_messages::STALE_TAILED,
            }),
        );
        let row = message(&app, wid, ChromeMessage::BandRow0);
        assert!(row.activates, "a band row activates");
        assert_eq!(
            row.capsules.len(),
            2,
            "the authored capsule, then the row's link"
        );
        assert_eq!(
            row.capsules[1].index,
            crate::a11y_tree::CAPSULE_DETAILS,
            "the link takes the Details slot"
        );
        let settings = |app: &App| {
            let (_, view) = app.active_native_view(wid)?;
            match app.native_runtime.view_state(view)? {
                crate::native_app::AppViewState::Settings(state) => {
                    Some((state.route, state.messages_selected))
                }
                _ => None,
            }
        };
        assert_eq!(settings(&app), None);
        // The row itself: the body press — its `Details ›`.
        app.activate_a11y_message(wid, ChromeMessage::BandRow0, row.context);
        assert_eq!(
            app.messages.live(id).unwrap().acted.map(|(k, _)| k),
            None,
            "the body is no capsule: nothing is recorded as one"
        );
        assert!(app.messages.live(id).unwrap().seen, "…but the row is seen");
        assert_eq!(
            settings(&app),
            Some((
                crate::native_settings::SettingsRoute::Messages,
                Some(id.raw())
            )),
            "Settings \u{25b8} Messages at this entry, in this window"
        );
        // The authored child: its own intent.
        app.activate_a11y_message(
            wid,
            ChromeMessage::BandCapsule { row: 0, index: 0 },
            row.context,
        );
        assert_eq!(
            app.messages.live(id).unwrap().acted.map(|(k, _)| k),
            Some(aterm_messages::ActionIndex(0)),
            "the engine recorded the press"
        );
        assert_eq!(
            settings(&app).map(|(route, _)| route),
            Some(crate::native_settings::SettingsRoute::Packages),
            "Settings \u{25b8} Packages, in this window"
        );
        // The Details child: the body press again.
        app.activate_a11y_message(
            wid,
            ChromeMessage::BandCapsule {
                row: 0,
                index: crate::a11y_tree::CAPSULE_DETAILS,
            },
            row.context,
        );
        assert_eq!(
            settings(&app).map(|(route, _)| route),
            Some(crate::native_settings::SettingsRoute::Messages)
        );
        // The row is gone: a stale activate names a slot the frame no longer carries.
        assert!(app.resolve_message(id, aterm_messages::Outcome::Ok));
        assert!(
            app.grid_a11y_messages(wid)
                .iter()
                .all(|m| m.message != ChromeMessage::BandRow0),
            "a resolved row is not published"
        );
        app.activate_a11y_message(wid, ChromeMessage::BandRow0, row.context);
        app.activate_a11y_message(
            wid,
            ChromeMessage::BandCapsule { row: 0, index: 0 },
            row.context,
        );
        assert!(app.messages.live(id).is_none());

        // Queue an actual published capsule, then replace the message in its slot.
        // No OS accessibility callback or handler is invoked by this captured route.
        let mut app = App::headless_for_test();
        app.prepare_terminal_capture_grid(wid).unwrap();
        let posting = |title: &str| {
            aterm_messages::Message::new(
                aterm_messages::tags::TOOLCHAIN,
                aterm_messages::Severity::Info,
                title,
            )
            .key("a11y-delayed")
            .action(aterm_messages::Intent::OpenSettings {
                route: "/packages".into(),
            })
            .hold(aterm_messages::Hold::Live {
                stale_after: aterm_messages::STALE_TAILED,
            })
        };
        let first = app.post_message(posting("First task"));
        let queued = |app: &mut App| {
            app.prepare_terminal_capture_grid(wid).unwrap();
            let tree = app.grid_a11y_tree(wid, None).expect("published grid");
            let (target, node) = tree
                .nodes
                .iter()
                .find(|(_, node)| {
                    node.role() == aterm_a11y::model::Role::Button
                        && node.label() == Some("Packages")
                })
                .expect("published authored capsule");
            aterm_a11y::model::ActionRequest {
                context: node.action_context(),
                action: aterm_a11y::model::Action::Click,
                target_tree: aterm_a11y::model::TreeId::ROOT,
                target_node: *target,
                data: None,
            }
        };
        let stale_owner = queued(&mut app);
        let second = app.post_message(posting("Second task"));
        assert_ne!(first, second);
        assert!(app.messages.live(first).is_none());
        app.on_accessibility_action(wid, stale_owner);
        assert_eq!(
            settings(&app),
            None,
            "old slot must not activate the new message"
        );
        assert!(app.messages.live(second).unwrap().acted.is_none());
        let stale_action = queued(&mut app);
        assert!(app.messages.restate(
            second,
            aterm_messages::Restatement {
                actions: Some(vec![aterm_messages::Intent::OpenSettings {
                    route: "/appearance".into()
                }]),
                ..aterm_messages::Restatement::default()
            },
            Instant::now()
        ));
        app.on_accessibility_action(wid, stale_action.clone());
        assert_eq!(
            settings(&app),
            None,
            "same id with changed actions is still stale"
        );
        assert!(app.messages.live(second).unwrap().acted.is_none());
        // Restore the original action: its earlier version stays retired.
        assert!(app.messages.restate(
            second,
            aterm_messages::Restatement {
                actions: Some(vec![aterm_messages::Intent::OpenSettings {
                    route: "/packages".into()
                }]),
                ..aterm_messages::Restatement::default()
            },
            Instant::now()
        ));
        app.on_accessibility_action(wid, stale_action);
        assert_eq!(
            settings(&app),
            None,
            "restoring the old action does not revive its queued version"
        );
        let current = queued(&mut app);
        for context in [
            None,
            Some(aterm_a11y::model::ActionContext::new(
                second.raw(),
                current.context.unwrap().generation(),
            )),
            Some(aterm_a11y::model::ActionContext::tab(second.raw())),
        ] {
            let mut wrong = current.clone();
            wrong.context = context;
            app.on_accessibility_action(wid, wrong);
            assert_eq!(settings(&app), None, "unbound or wrong-domain action");
        }
        app.on_accessibility_action(wid, current);
        assert_eq!(
            settings(&app).map(|(route, _)| route),
            Some(crate::native_settings::SettingsRoute::Packages)
        );
        assert_eq!(
            app.messages
                .live(second)
                .unwrap()
                .acted
                .map(|(index, _)| index),
            Some(aterm_messages::ActionIndex(0))
        );
    }

    /// An a11y activate on any committed band row opens that row's `Details ›` (Settings
    /// ▸ Messages at that entry in this window), reads the row (a held one folds; the row
    /// count holds through its quiet) and leaves its neighbour alone. An activate on a
    /// slot no longer carried does nothing; the overflow link's activate opens the page
    /// unselected and reads no row, and is stale without an overflow row.
    #[test]
    fn a11y_activate_on_a_row_opens_details() {
        let settings = |app: &App, wid: WindowId| {
            let (_, view) = app.active_native_view(wid)?;
            match app.native_runtime.view_state(view)? {
                crate::native_app::AppViewState::Settings(state) => {
                    Some((state.route, state.messages_selected))
                }
                _ => None,
            }
        };
        let row_of = |app: &App, wid: WindowId, id: aterm_messages::MessageId| {
            let cols = usize::from(app.windows[&wid].cols);
            app.band_presentation(cols)
                .rows
                .iter()
                .position(|r| matches!(r.kind, aterm_messages::RowKind::Message(m) if m == id))
        };
        let slots = [
            ChromeMessage::BandRow0,
            ChromeMessage::BandRow1,
            ChromeMessage::BandRow2,
        ];
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.prepare_terminal_capture_grid(wid).unwrap();
        let meter = app.post_message(
            aterm_messages::Message::new(
                aterm_messages::tags::TOOLCHAIN,
                aterm_messages::Severity::Info,
                "Installing ALab toolchain",
            )
            .line("trust — extracting")
            .action(aterm_messages::Intent::OpenSettings {
                route: "/packages".into(),
            })
            .hold(aterm_messages::Hold::Live {
                stale_after: aterm_messages::STALE_TAILED,
            }),
        );
        let warning = app.post_message(
            aterm_messages::Message::new(
                aterm_messages::tags::CONFIG,
                aterm_messages::Severity::Warn,
                "aterm.toml: unknown key `foo`",
            )
            .line("at line 12"),
        );
        assert_eq!(app.message_band_rows, 2);
        let row = row_of(&app, wid, warning).expect("the warning is on the band");
        let node = message(&app, wid, slots[row]);
        assert!(node.activates, "a band row activates");
        assert_eq!(
            node.capsules.iter().map(|c| c.index).collect::<Vec<_>>(),
            [crate::a11y_tree::CAPSULE_DETAILS],
            "a capsule-less row publishes its link alone"
        );
        assert_eq!(settings(&app, wid), None);
        app.activate_a11y_message(wid, slots[row], node.context);
        assert_eq!(
            settings(&app, wid),
            Some((
                crate::native_settings::SettingsRoute::Messages,
                Some(warning.raw())
            )),
            "Settings \u{25b8} Messages at this entry, in this window"
        );
        assert!(
            app.messages.live(warning).is_none(),
            "read, so a held row folds"
        );
        assert!(
            app.messages
                .live(meter)
                .is_some_and(|l| !l.seen && l.acted.is_none()),
            "the row beside it is untouched"
        );
        assert!(
            row_of(&app, wid, warning).is_none(),
            "the folded row is off the glass"
        );
        assert_eq!(
            app.message_band_rows, 2,
            "the count holds through its quiet (D1's hysteresis shrinks late): an \
             activate never re-grids the terminal under the reader"
        );
        assert!(
            app.grid_a11y_messages(wid)
                .iter()
                .all(|m| m.message != ChromeMessage::BandRow1),
            "the second slot is no longer published"
        );
        // A stale activate on the slot the fold emptied: nothing.
        app.activate_a11y_message(wid, ChromeMessage::BandRow1, node.context);
        assert_eq!(
            settings(&app, wid),
            Some((
                crate::native_settings::SettingsRoute::Messages,
                Some(warning.raw())
            ))
        );
        assert!(app.messages.live(meter).is_some_and(|l| !l.seen));

        // The overflow link, in a fresh window: the page, nothing selected,
        // no row read, the band as it was.
        let mut app = App::headless_for_test();
        app.prepare_terminal_capture_grid(wid).unwrap();
        let ids: Vec<_> = (0..5)
            .map(|i| {
                app.post_message(
                    aterm_messages::Message::new(
                        aterm_messages::tags::CONFIG,
                        aterm_messages::Severity::Warn,
                        format!("config warning {i}"),
                    )
                    .line("why"),
                )
            })
            .collect();
        assert_eq!(app.message_band_rows, aterm_messages::MAX_ROWS);
        let link = message(&app, wid, ChromeMessage::BandOverflow);
        assert!(link.activates, "the overflow row is a link");
        assert!(
            link.capsules.is_empty(),
            "the node is the one link: its painted chip is no second control"
        );
        app.activate_a11y_message(wid, ChromeMessage::BandOverflow, link.context);
        assert_eq!(
            settings(&app, wid),
            Some((crate::native_settings::SettingsRoute::Messages, None)),
            "the page, top of the log, nothing expanded"
        );
        for id in &ids {
            assert!(
                app.messages.live(*id).is_some_and(|l| !l.seen),
                "a link to the log reads no row"
            );
        }
        assert_eq!(app.message_band_rows, aterm_messages::MAX_ROWS);

        // No overflow row: the link's activate is stale and opens nothing.
        let mut app = App::headless_for_test();
        app.prepare_terminal_capture_grid(wid).unwrap();
        app.post_message(
            aterm_messages::Message::new(
                aterm_messages::tags::CONFIG,
                aterm_messages::Severity::Warn,
                "one warning",
            )
            .line("why"),
        );
        assert!(
            app.grid_a11y_messages(wid)
                .iter()
                .all(|m| m.message != ChromeMessage::BandOverflow),
            "one row: no overflow link is published"
        );
        app.activate_a11y_message(wid, ChromeMessage::BandOverflow, link.context);
        assert_eq!(settings(&app, wid), None, "nothing opened");
    }

    /// A countdown never re-announces a row: a download's latched ETA rides the
    /// description as a reader says it ("about … left"); the spoken name is the title
    /// alone.
    #[test]
    fn a_countdown_never_reannounces_a_row() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.prepare_terminal_capture_grid(wid).unwrap();
        let total: u64 = 200_000_000;
        let words = |done: u64| {
            crate::update_words::progress(
                &aterm_update::Progress::Downloading {
                    version: "0.91.0".to_string(),
                    bytes_done: done,
                    bytes_total: total,
                },
                None,
                "",
            )
        };
        // Ten seconds of a steady 5 MB/s, fed at the instants they arrived.
        let now = Instant::now();
        let base = now - std::time::Duration::from_secs(10);
        let id = app
            .messages
            .post(words(0), crate::messages_host::wall_stamp_now(), base)
            .id;
        for k in 1..=20u64 {
            let t = base + std::time::Duration::from_millis(k * 500);
            app.messages.restate(
                id,
                crate::messages_host::restatement_of(&words(k * 2_500_000)),
                t,
            );
        }
        app.settle_messages(now);
        assert_eq!(app.message_band_rows, 1);
        let first = message(&app, wid, ChromeMessage::BandRow0);
        app.messages.restate(
            id,
            crate::messages_host::restatement_of(&words(60_000_000)),
            now,
        );
        let later = message(&app, wid, ChromeMessage::BandRow0);
        assert_eq!(later.text, first.text, "the countdown is never announced");
    }
}
