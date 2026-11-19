// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Host half of the System Resources page ([`crate::native_resources`]): opening its one view in
//! the front window from View ▸ System Resources…, the palette or `aterm ctl open app
//! system-resources`, scoped to the terminal tab in front at the open, handing it each reading
//! the stream adopts (where a chosen session that ended returns its view to this Mac), and
//! saying whether it is on screen, which keeps the stream at the focused cadence
//! ([`crate::App::vitals_audience`]).
//!
//! The CPU document's Collect CPU Policy and Cancel are a person's presses of one view: the
//! collection is bound to that view and ends, its password sheet with it, once the view is not
//! its window's front tab in a window that is not occluded (SD-83). The Spotlight document's
//! buttons (SD-96) and the macOS configuration section's Apply… (SD-97) run root steps bound the
//! same way ([`crate::root_step_authorization`]).

use std::time::Instant;

use crate::native_app::{AppIcon, AppKind, AppViewState, DamageRegion, NativeApp};
use crate::native_resources::{RESOURCES_TITLE, resources_tooltip};
use crate::native_ui::UiKey;
use crate::tab_model::{TabId, View, ViewId};

impl crate::App {
    /// Whether `view` is a System Resources view.
    fn is_resources_view(&self, view: ViewId) -> bool {
        matches!(self.view_store.get(view), Some(View::Native(native))
            if self.native_runtime.app(native.instance)
                .is_some_and(|app| app.kind() == AppKind::SystemResources))
    }

    /// Whether a System Resources view is on screen ([`Self::view_on_screen`]): the front tab of
    /// a window that is not occluded shows it.
    pub(crate) fn resources_page_on_screen(&self) -> bool {
        self.view_on_screen(|view| self.is_resources_view(view))
    }

    /// [`Self::open_resources_at`] with no section, as a test opens the page.
    #[cfg(test)]
    pub(crate) fn open_resources_tab(&mut self) -> Result<(), String> {
        self.open_resources_at(None)
    }

    /// `aterm ctl open app system-resources [section=<word>]`: focus the front window's System
    /// Resources view, or open one there as a tab, and bring the block keyed `reveal` (a section's
    /// heading) to the top of the view at its next layout. Never moves the person to another
    /// window.
    pub(crate) fn open_resources_at(&mut self, reveal: Option<UiKey>) -> Result<(), String> {
        // Observe the prior visibility before switching, even if no reading arrived while hidden.
        self.resources_visibility_changed();
        let wid = self
            .frontmost_window
            .ok_or_else(|| "no window to open System Resources in".to_string())?;
        let existing = self.windows.get(&wid).and_then(|window| {
            window.tab_set.tabs().iter().find_map(|tab| {
                let mut found = None;
                tab.root.any_leaf(&mut |&view| {
                    if self.is_resources_view(view) {
                        found = Some((tab.id, view));
                        true
                    } else {
                        false
                    }
                });
                found
            })
        });
        let (tab, view) = match existing {
            Some(placed) => placed,
            None => {
                // A new view starts scoped to the terminal tab in front, else to this Mac.
                let scope = crate::native_resources::PageScope::opened(
                    self.deck_tab_panes(wid),
                    self.deck_tab(wid).1,
                );
                self.stage_resources_tab(wid, scope)?
            }
        };
        let mut route_guard_window = self
            .windows
            .get_mut(&wid)
            .ok_or_else(|| "the window closed".to_string())?;
        let window = &mut **route_guard_window;
        if !window.tab_set.switch_to(tab)
            || !window
                .tab_set
                .active_mut()
                .is_some_and(|tab| tab.set_focus(view))
        {
            return Err("System Resources could not be shown in the requesting window".into());
        }
        crate::app_tabs::cancel_pending_close(window);
        window.last_present = None;
        drop(route_guard_window);
        self.resync_active_or_window(wid);
        self.sync_active_session();
        // The page reads at the focused cadence from here on; ask now, so its first frame is not
        // a background cadence's reading old.
        self.publish_resources();
        if let Some(key) = reveal {
            let marked = match self.native_runtime.view_state_mut(view).as_deref_mut() {
                Some(crate::native_app::AppViewState::SystemResources(state)) => {
                    state.reveal_on_lay(key);
                    true
                }
                _ => false,
            };
            if marked {
                self.invalidate_native_view_cache(wid, view, DamageRegion::All);
            }
        }
        self.vitals_ask_now(Instant::now());
        self.request_redraw_all_windows();
        Ok(())
    }

    /// View ▸ System Resources… and its palette row (`MenuAction::SystemResources`).
    pub(crate) fn open_resources_from_menu(&mut self) {
        if let Err(why) =
            self.open_app(crate::app_control::OpenRequest::SystemResources { section: None })
        {
            self.pending_action_refusal = Some(why);
        }
    }

    /// Mint a view of the System Resources page scoped to `scope` as a tab of `target`. One page
    /// serves every view.
    pub(crate) fn stage_resources_tab(
        &mut self,
        target: impl Into<crate::app_tabs::NativeInstallTarget>,
        scope: crate::native_resources::PageScope,
    ) -> Result<(TabId, ViewId), String> {
        let target = target.into();
        let presentation = crate::tab_model::TabPresentation {
            title: RESOURCES_TITLE.into(),
            icon: Some(AppIcon::SystemResources),
            indicators: crate::tab_model::TabIndicators::default(),
            conn: None,
            closable: true,
            tooltip: Some(resources_tooltip()),
        };
        let state = AppViewState::SystemResources(Box::new(
            crate::native_resources::ResourcesViewState::scoped(scope),
        ));
        let installed = if let Some(instance) = self
            .native_runtime
            .instance_by_kind(AppKind::SystemResources)
        {
            self.install_native_tab(target, instance, state, presentation)
        } else {
            self.install_new_native_tab(
                target,
                NativeApp::SystemResources(Box::default()),
                state,
                presentation,
            )
            .map(|(_, tab, view)| (tab, view))
        };
        installed.map_err(|error| format!("System Resources could not be opened: {error:?}"))
    }

    /// A person's press of the CPU document's policy buttons on `view` in `wid`
    /// ([`crate::native_app::AppEffect::CpuPolicy`]): Collect binds one collection to the view,
    /// Cancel ends the pending one and its sheet. The page then says what is pending.
    pub(crate) fn press_cpu_policy(
        &mut self,
        wid: crate::WindowId,
        view: ViewId,
        press: crate::native_app::CpuPolicyPress,
    ) {
        #[cfg(target_os = "macos")]
        match press {
            crate::native_app::CpuPolicyPress::Collect => {
                let starter = self.cpu_policy_starter();
                if let Err(refusal) = self.collect_cpu_policy_from_view(wid, view, starter) {
                    aterm_log::info!(
                        "resources: CPU policy was not collected: {}",
                        refusal.words()
                    );
                }
            }
            crate::native_app::CpuPolicyPress::Cancel => self.cancel_cpu_policy_from_page(),
        }
        #[cfg(not(target_os = "macos"))]
        let _ = (wid, view, press);
        self.publish_cpu_policy_state();
    }

    /// Tell the page whether a CPU policy collection is pending, and repaint the views of it on
    /// screen when that changed.
    pub(crate) fn publish_cpu_policy_state(&mut self) {
        let Some(instance) = self
            .native_runtime
            .instance_by_kind(AppKind::SystemResources)
        else {
            return;
        };
        #[cfg(target_os = "macos")]
        let pending = self.cpu_policy_pending();
        #[cfg(not(target_os = "macos"))]
        let pending = false;
        if self.native_runtime.resources_policy(instance, pending) {
            self.repaint_resources_views(instance);
        }
    }

    /// Remember the page's current visibility and start the Disk survey on a rising edge. A
    /// view that is no longer in front ends the CPU policy collection or root step it started.
    /// On the rising edge the macOS configuration section reads at once.
    pub(crate) fn resources_visibility_changed(&mut self) {
        #[cfg(target_os = "macos")]
        self.withdraw_askpass_view_unless_front();
        if let Some(instance) = self
            .native_runtime
            .instance_by_kind(AppKind::SystemResources)
        {
            let on_screen = self.resources_page_on_screen();
            if self.native_runtime.resources_seen(instance, on_screen) {
                self.disk_lens_opened();
                self.vitals.macos_config.force();
            }
        }
    }

    /// A person's Apply… of the macOS configuration section on `view` in `wid`
    /// ([`crate::native_app::AppEffect::MacosConfiguration`]): a root step runs under the
    /// password sheet bound to the view, fseventsd's restart opens its tab, and the build folders
    /// Time Machine backs up are marked. What it did is said under its row.
    pub(crate) fn press_macos_configuration(
        &mut self,
        wid: crate::WindowId,
        view: ViewId,
        act: crate::resources_macos::Act,
    ) {
        use crate::resources_macos::{Act, Tm, slug_of};
        let slug = slug_of(act);
        let said = match act {
            #[cfg(target_os = "macos")]
            Act::Root(step) => match self.start_root_step(wid, view, step, None) {
                Ok(()) => "Waiting for your Mac password\u{2026}".to_owned(),
                Err(refusal) => format!("Not applied: {}", refusal.words()),
            },
            #[cfg(not(target_os = "macos"))]
            Act::Root(_) => {
                let _ = view;
                "Not applied: this needs macOS".to_owned()
            }
            Act::RestartFseventsd => {
                if self.restart_service(wid, aterm_vitals::services::FSEVENTSD.name) {
                    "A tab opened to restart fseventsd; type your Mac password there".to_owned()
                } else {
                    crate::services_watch::take_refusal()
                        .unwrap_or_else(|| "Opened nothing".to_owned())
                }
            }
            Act::MarkBuildFolders => {
                let slot = std::sync::Arc::clone(&self.vitals.macos_config);
                let inside: Vec<std::path::PathBuf> = slot
                    .reading()
                    .map(|(reading, _)| {
                        reading
                            .build_folders
                            .iter()
                            .filter(|(_, tm)| *tm == Tm::In)
                            .map(|(path, _)| path.clone())
                            .collect()
                    })
                    .unwrap_or_default();
                // One extended attribute a folder, never under a folder macOS guards (the
                // reading names those Guarded, never In).
                let failed: Vec<String> = inside
                    .iter()
                    .filter_map(|path| {
                        (slot.port().mark)(path)
                            .err()
                            .map(|error| format!("{}: {error}", path.display()))
                    })
                    .collect();
                slot.force();
                if failed.is_empty() {
                    format!(
                        "Marked {} build {} out of Time Machine",
                        inside.len(),
                        if inside.len() == 1 {
                            "folder"
                        } else {
                            "folders"
                        }
                    )
                } else {
                    format!("Not marked: {}", failed.join("; "))
                }
            }
        };
        aterm_log::info!("resources: macOS configuration {slug}: {said}");
        self.vitals.macos_config.say(slug, said);
        self.publish_resources();
    }

    /// A person's press of one of the Spotlight document's buttons on `view` in `wid`
    /// ([`crate::native_app::AppEffect::SpotlightControl`]): its root step runs under the
    /// password sheet bound to the view, a keep-out's folder first chosen in the open panel and
    /// judged by its words alone ([`aterm_types::macos_setup::spotlight_scope::keep_out`]). What
    /// it did is said under the buttons.
    pub(crate) fn press_spotlight_control(
        &mut self,
        wid: crate::WindowId,
        view: ViewId,
        step: aterm_types::macos_setup::root_only::Step,
    ) {
        let said = self.start_spotlight_control(wid, view, step);
        aterm_log::info!("resources: Spotlight {}: {said}", step.word());
        self.vitals.spotlight_scope.say(said);
        self.publish_resources();
    }

    /// [`Self::press_spotlight_control`]'s start, in the words the page says.
    fn start_spotlight_control(
        &mut self,
        wid: crate::WindowId,
        view: ViewId,
        step: aterm_types::macos_setup::root_only::Step,
    ) -> String {
        #[cfg(target_os = "macos")]
        {
            use aterm_types::macos_setup::spotlight_scope;
            let folder = if step.takes_folder() {
                let Some(picked) =
                    (self.folder_picker)("Keep a folder out of Spotlight", "Keep Out")
                else {
                    return "No folder was chosen".to_owned();
                };
                let Some((scope, _)) = self.vitals.spotlight_scope.reading() else {
                    return "Not run: this Mac's Spotlight scope is not read yet".to_owned();
                };
                let Some(home) = scope.listing.home.as_deref() else {
                    return "Not run: the home folder is not read yet".to_owned();
                };
                match spotlight_scope::keep_out(home, &scope.listing.yours, &picked) {
                    Ok(folder) => Some(folder),
                    Err(why) => {
                        return format!("Not kept out: {}: {why}", scope.listing.shown(&picked));
                    }
                }
            } else {
                None
            };
            match self.start_root_step(wid, view, step, folder) {
                Ok(()) => "Waiting for your Mac password\u{2026}".to_owned(),
                Err(refusal) => format!("Not run: {}", refusal.words()),
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (wid, view, step);
            "Not run: Spotlight is macOS's".to_owned()
        }
    }

    /// A root step the page started ended: say how where it was pressed (under the macOS
    /// configuration row, or under the Spotlight document's buttons; the scope's Apply under
    /// both, since either offers it) and read again.
    #[cfg(target_os = "macos")]
    pub(crate) fn root_step_ended(
        &mut self,
        step: aterm_types::macos_setup::root_only::Step,
        ended: &crate::root_step_authorization::Ended,
    ) {
        use crate::resources_macos::{Act, SPOTLIGHT_DOCUMENT, slug_of};
        use crate::root_step_authorization::Ended;
        let said = match ended {
            Ended::Applied => "Applied".to_owned(),
            Ended::NotApplied(why) => format!("Not applied: {why}"),
            Ended::NotRun(why) => format!("Not run: {why}"),
        };
        aterm_log::info!("resources: root step {}: {said}", step.word());
        let slug = slug_of(Act::Root(step));
        if slug != SPOTLIGHT_DOCUMENT {
            self.vitals.macos_config.say(slug, said.clone());
        }
        if step.spotlight() {
            self.vitals.spotlight_scope.say(said);
            self.vitals.spotlight_scope.relist_now();
        }
        self.vitals.macos_config.force();
        self.publish_resources();
    }

    /// Tell the page its Spotlight document buttons' state, and repaint the views of it on
    /// screen when that changed.
    fn publish_spotlight_controls(&mut self, instance: crate::native_app::AppInstanceId) {
        #[cfg(target_os = "macos")]
        let pending = self.root_step_pending();
        #[cfg(not(target_os = "macos"))]
        let pending = None;
        let page = crate::native_resources::SpotlightPage {
            pending,
            said: self.vitals.spotlight_scope.said(),
        };
        if self.native_runtime.resources_spotlight(instance, page) {
            self.repaint_resources_views(instance);
        }
    }

    /// Tell the page the macOS configuration section's state, and repaint the views of it on
    /// screen when that changed.
    fn publish_macos_configuration(&mut self, instance: crate::native_app::AppInstanceId) {
        #[cfg(target_os = "macos")]
        let pending = self
            .root_step_pending()
            .map(|step| crate::resources_macos::slug_of(crate::resources_macos::Act::Root(step)));
        #[cfg(not(target_os = "macos"))]
        let pending = None;
        let status = self.system_state_host.latest_status();
        let selection = status
            .plan
            .as_ref()
            .map_or_else(aterm_types::macos_setup::Selection::none, |plan| {
                plan.selection
            });
        let page = crate::resources_macos::MacosPage {
            reading: self.vitals.macos_config.reading(),
            alab: crate::system_state::lines::alab_lockup(status, selection),
            pending,
            said: self.vitals.macos_config.said(),
        };
        if self.native_runtime.resources_macos(instance, page) {
            self.repaint_resources_views(instance);
        }
    }

    /// Tick the Spotlight document's scope probe at `now` ([`ScopeSlot::service`]): only while
    /// the page is on screen, never behind another tab or in an occluded window.
    ///
    /// [`ScopeSlot::service`]: crate::resources_spotlight::ScopeSlot::service
    pub(crate) fn service_spotlight_probe(&mut self, now: Instant) {
        let shown = self.resources_page_on_screen();
        let volume = self
            .vitals
            .snapshot
            .as_deref()
            .and_then(crate::resources_spotlight::data_volume_read);
        self.vitals.spotlight_scope.note_volume(volume);
        self.vitals.spotlight_scope.service(shown, now);
    }

    /// Hand the page the reading the stream last adopted and the Spotlight scope as last probed,
    /// and repaint the views of it on screen.
    pub(crate) fn publish_resources(&mut self) {
        let Some(instance) = self
            .native_runtime
            .instance_by_kind(AppKind::SystemResources)
        else {
            return;
        };
        // Track visibility before checking the reading: opening and hiding the page are edges
        // even when no new sample has arrived. The survey has its own minimum interval.
        self.resources_visibility_changed();
        self.service_spotlight_probe(Instant::now());
        // System State's Spotlight indexing row reads the folders this probe finds taken back.
        self.republish_system_state_spotlight();
        let shown = self.resources_page_on_screen();
        self.vitals.macos_config.service(shown, Instant::now());
        self.publish_cpu_policy_state();
        self.publish_macos_configuration(instance);
        self.publish_spotlight_controls(instance);
        let Some(snap) = self.vitals.snapshot.clone() else {
            return;
        };
        let scope = self.vitals.spotlight_scope.reading();
        if !self.native_runtime.publish_resources(instance, snap, scope) {
            return;
        }
        self.repaint_resources_views(instance);
    }

    /// Repaint the views of the page `instance` that are on screen.
    fn repaint_resources_views(&mut self, instance: crate::native_app::AppInstanceId) {
        let mut visible = Vec::new();
        for (&wid, window) in &self.windows {
            if window.occluded {
                continue;
            }
            if let Some(tab) = window.tab_set.active() {
                tab.root.visit(&mut |&view| {
                    if (!tab.zoomed || tab.focus == view)
                        && matches!(
                            self.view_store.get(view),
                            Some(View::Native(native)) if native.instance == instance
                        )
                    {
                        visible.push((wid, view));
                    }
                });
            }
        }
        for (wid, view) in visible {
            self.invalidate_native_view_cache(wid, view, DamageRegion::All);
            if let Some(window) = self.windows.get(&wid) {
                window.request_redraw();
            }
        }
    }
}

#[cfg(test)]
#[path = "app_resources_tests.rs"]
pub(crate) mod tests;
