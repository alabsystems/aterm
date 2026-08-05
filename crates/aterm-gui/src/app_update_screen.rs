// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `App` glue for the UPDATE FLOW: the ONE-CLICK apply every "update ready" affordance
//! fires ([`App::apply_update_or_details`] — the Version menu's ⬆️ item, the palette's
//! Version row, the notice pill, the off-macOS tab-strip ↻), the live Version-menu
//! re-sync ([`App::refresh_version_menu`]), and the process updater state projected into
//! the native Settings `/updates` route. The retired modal remains test scaffolding only
//! until its low-level painter/input regression tests are migrated.

use crate::App;
use crate::native_app::UpdateOutcome;
use crate::update_screen::{UpdateHit, UpdateState};

impl App {
    /// Snapshot the current process-owned updater reducer into a fresh [`UpdateState`].
    /// This is memory-only: ledger and installed-bundle facts enter the reducer solely
    /// through typed worker completions, so Settings/introspection never block input.
    /// `checking` marks a manual check as in flight.
    pub(crate) fn update_snapshot(&self, checking: bool) -> UpdateState {
        UpdateState::from_service(self.native_updater_service.snapshot(), checking)
    }

    /// Canonical human "Check for Updates…" gesture: reveal the durable Settings
    /// route, then request work from the process-owned updater. The service returns
    /// `Joined` for an in-flight ticket, so repeated menu/compatibility gestures never
    /// create a second physical check. Explicit `open app settings /updates` remains
    /// navigation-only and does not call this method.
    pub(crate) fn open_software_update_route_and_check(&mut self) -> Result<UpdateOutcome, String> {
        if !self.open_settings_tab(crate::native_settings::SettingsRoute::SoftwareUpdate) {
            return Err("could not open the native Software Update route".to_string());
        }
        Ok(self.start_native_update_check())
    }

    /// Close the Software Update overlay on window `wid` (no-op if not open there).
    pub(crate) fn update_screen_exit(&mut self, wid: crate::WindowId) {
        if let Some(ws) = self.windows.get_mut(&wid)
            && ws.update_screen().is_some()
        {
            ws.overlay = None;
            if let Some(w) = &ws.os_window {
                w.request_redraw();
            }
        }
        // Publish the now-empty tree (the overlay closed).
        self.overlay_a11y_update();
    }

    /// Refresh an OPEN Software Update overlay from the current on-disk status (reached from
    /// the native updater completion reducer after a manual check finishes) — clears the
    /// "Checking…" state and shows any freshly-staged build + notes. No-op if the overlay
    /// closed meanwhile. Refreshes EVERY window that has it open (front can lag).
    #[cfg(test)]
    pub(crate) fn update_screen_refresh(&mut self) {
        let snap_open: Vec<crate::WindowId> = self
            .windows
            .iter()
            .filter(|(_, ws)| ws.update_screen().is_some())
            .map(|(id, _)| *id)
            .collect();
        for wid in snap_open {
            let snap = self.update_snapshot(false);
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.overlay = Some(crate::overlay::Overlay::Update(snap));
                if let Some(w) = &ws.os_window {
                    w.request_redraw();
                }
            }
        }
        // A fresh check may have staged a new build: refresh the accessibility tree.
        self.overlay_a11y_update();
    }

    /// Run ONE update check off the event loop, marking the open overlay "Checking…" first,
    /// then joining the process-global native updater service. Repeated requests
    /// subscribe to its revision and never spawn a second physical worker.
    pub(crate) fn update_screen_check(&mut self) {
        #[cfg(not(test))]
        {
            let _ = self.start_native_update_check();
        }
        #[cfg(test)]
        {
            let Some(wid) = self.frontmost_window else {
                return;
            };
            // Reflect "Checking…" immediately.
            let checking = self.update_snapshot(true);
            if let Some(ws) = self.windows.get_mut(&wid) {
                if ws.update_screen().is_none() {
                    return;
                }
                ws.overlay = Some(crate::overlay::Overlay::Update(checking));
                if let Some(w) = &ws.os_window {
                    w.request_redraw();
                }
            }
            // Reflect the "Checking…" state to a screen reader too.
            self.overlay_a11y_update();
            let _ = self.start_native_update_check();
        }
    }

    /// TRUE iff the process reducer owns a verified, STRICTLY-newer staged build.
    /// Callers that originate outside the updater worker reconcile the durable ledger
    /// once before consulting this snapshot; keeping this predicate memory-only avoids
    /// repeated filesystem work in one menu click. Mirrors the
    /// `ATERM_DEBUG_SEAMLESS_REEXEC` QA seam so the handoff remains exercisable.
    pub(crate) fn staged_update_ready(&self) -> bool {
        if std::env::var_os("ATERM_DEBUG_SEAMLESS_REEXEC").is_some() {
            return true;
        }
        let snapshot = self.native_updater_service.snapshot();
        snapshot.phase == crate::native_updater_service::UpdaterPhase::Staged
            && snapshot
                .staged
                .as_ref()
                .is_some_and(|staged| staged.build > snapshot.current_build)
    }

    /// ONE-CLICK UPDATE (the owner's "click-upgrade" ask): every "update ready"
    /// affordance — the Version menu's ⬆️ item ([`crate::menu::MenuAction::ApplyUpdate`]),
    /// the palette's Version row, the "Update ready" notice pill, the off-macOS
    /// tab-strip ↻ — lands here. A strictly-newer STAGED build applies IMMEDIATELY via
    /// the process updater's one-shot apply authorization (which consumes the same
    /// `apply_staged_update_now` path after close preflight) — no intermediate overlay.
    /// With nothing actually staged
    /// (a stale nudge, the `ATERM_DEBUG_RELAUNCH_NUDGE` QA seam, a ledger cleared under
    /// us) it opens the Software Update route in the native Settings tab: honest
    /// details, never a dead click, a legacy modal, or a blind restart.
    pub(crate) fn apply_update_or_details(&mut self) {
        let debug_seamless = std::env::var_os("ATERM_DEBUG_SEAMLESS_REEXEC").is_some();
        let staged_ready = self.staged_update_ready();
        let _ = self.apply_update_or_details_with_facts(staged_ready, debug_seamless);
    }

    /// Exact menu-action reducer with environment/disk observations supplied once.
    /// Keeping mechanics behind this seam lets tests drive the genuinely blocked
    /// click path without mutating process-global environment or fabricating a ledger.
    fn apply_update_or_details_with_facts(
        &mut self,
        staged_ready: bool,
        debug_seamless: bool,
    ) -> Option<crate::native_app::UpdateOutcome> {
        if staged_ready {
            let outcome = if debug_seamless {
                self.apply_debug_seamless_update()
            } else {
                self.apply_native_update(crate::native_updater_service::ApplyMode::Immediate)
            };
            self.surface_update_apply_outcome("manual", outcome.clone(), true);
            Some(outcome)
        } else {
            let _ = self.open_settings_tab(crate::native_settings::SettingsRoute::SoftwareUpdate);
            None
        }
    }

    /// Make every returned updater outcome observable. A manual click opens the native
    /// Software Update route on failure/block so the enabled menu item can never look
    /// inert; automatic/control paths retain the non-disruptive notification + log.
    pub(crate) fn surface_update_apply_outcome(
        &mut self,
        source: &str,
        outcome: crate::native_app::UpdateOutcome,
        open_details: bool,
    ) {
        // Persist the apply-lane verdict into the updater's own health ledger and
        // status file BEFORE any UI reaction. Until this existed the apply lane was
        // invisible to `aterm-ctl update status`: a handoff could fail every single
        // time for three releases while `health.toml` stayed all-zero and status
        // said "up to date", because only the download lane was ever recorded.
        //
        // `Deferred`/`Blocked` mean "not yet, conditions were not met" — normal and
        // self-correcting, so they must NOT touch the failure streak: doing that
        // would manufacture an escalation every time the user happened to be typing.
        //
        // But silent was the wrong other extreme, and it is what the owner actually
        // hit: a staged build sat unapplied across two releases while `update
        // status` reported `failing=0 failing_applies=0` and advised a relaunch,
        // because the refusal reached this arm and stopped here. "Nothing is wrong"
        // and "we declined, here is why" are different answers and the file could
        // only say the first. `record_apply_refusal` is the separate, non-streak
        // slot for the second; it is expiry-bound to the RUNNING build, since a
        // successful in-session apply execs away and never returns to clear it.
        let current_build = self.native_updater_service.snapshot().current_build;
        match &outcome {
            crate::native_app::UpdateOutcome::Failed { message } => {
                aterm_update::record_apply_failure(current_build, message);
            }
            crate::native_app::UpdateOutcome::Accepted => {
                aterm_update::record_apply_success(current_build);
            }
            crate::native_app::UpdateOutcome::Blocked { reasons } => {
                aterm_update::record_apply_refusal(current_build, &reasons.join(" · "));
            }
            crate::native_app::UpdateOutcome::Deferred { reason } => {
                aterm_update::record_apply_refusal(current_build, reason);
            }
            // Installed-but-needs-relaunch is neither a refusal nor a completion:
            // the bytes ARE in place and the next launch runs them, which the
            // status line already reports from the staged marker.
            crate::native_app::UpdateOutcome::InstalledNeedsRelaunch { .. } => {}
        }
        match outcome {
            crate::native_app::UpdateOutcome::Accepted => {
                aterm_log::info!("update apply ({source}): accepted");
            }
            crate::native_app::UpdateOutcome::InstalledNeedsRelaunch { build, message } => {
                aterm_log::warn!(
                    "update apply ({source}): build {build} is installed; relaunch still needed: {message}"
                );
                if open_details {
                    let _ = self
                        .open_settings_tab(crate::native_settings::SettingsRoute::SoftwareUpdate);
                } else {
                    self.surface_nonmodal_update_status("↑ Update installed — relaunch once");
                }
            }
            crate::native_app::UpdateOutcome::Deferred { reason } => {
                aterm_log::info!("update apply ({source}) deferred: {reason}");
                if open_details {
                    let _ = self
                        .open_settings_tab(crate::native_settings::SettingsRoute::SoftwareUpdate);
                }
            }
            crate::native_app::UpdateOutcome::Blocked { reasons } => {
                let message = reasons.join(" · ");
                aterm_log::warn!("update apply ({source}) waiting: {message}");
                if open_details {
                    let _ = self
                        .open_settings_tab(crate::native_settings::SettingsRoute::SoftwareUpdate);
                } else {
                    self.surface_nonmodal_update_status("↑ Update waiting — see Version menu");
                }
            }
            crate::native_app::UpdateOutcome::Failed { message } => {
                aterm_log::warn!("update apply ({source}) failed safely: {message}");
                if open_details {
                    let _ = self
                        .open_settings_tab(crate::native_settings::SettingsRoute::SoftwareUpdate);
                } else if self.native_updater_service.snapshot().staged.is_some() {
                    self.surface_nonmodal_update_status("↑ Update paused — manual retry");
                } else {
                    // A retired/consumed artifact is not "still ready". Keep the
                    // status honest while the native Settings reducer carries detail.
                    self.surface_nonmodal_update_status("Update stopped safely — see details");
                }
            }
        }
    }

    /// Automatic/background updater status must never enter AppKit's nested modal
    /// loop. Paint a short in-app pill and leave full detail in native Settings/logs.
    pub(crate) fn surface_nonmodal_update_status(&mut self, text: &str) {
        self.notice = Some(crate::notice::TransientNotice::update_status(
            text,
            std::time::Instant::now(),
        ));
        self.request_redraw_all_windows();
    }

    /// Re-sync the macOS VERSION menu (the rightmost menu-bar title) to the live update
    /// state: `v<cur> ⬆️` + the one-click "Update to v<staged> — restart now" first item
    /// while a strictly-newer build is staged; `v<cur> ⬆️` + the "Updated to v<cur> just
    /// now" celebration item while the post-update realized arrow is live; plain
    /// `v<cur>` otherwise. The PERSISTENT update affordance lives here now (the titlebar
    /// "Update" capsule is retired). A no-op headless / off macOS (no menu handle).
    /// Call on every transition: `Wake::UpdateStaged`, the JUST_UPDATED boot, and the
    /// realized-arrow TTL expiry sweep in `about_to_wait`.
    pub(crate) fn refresh_version_menu(&self) {
        if let Some(handle) = self._menu.as_ref() {
            let staged = self.relaunch.as_ref().map(|r| (r.build, r.version.clone()));
            let realized = !self.serious_mode_enabled()
                && self
                    .upgrade_realized
                    .is_some_and(|t| t.elapsed() < crate::relaunch_notice::REALIZED_ARROW_TTL);
            crate::menu::update_version_menu(
                handle,
                staged.as_ref().map(|(b, v)| (*b, v.as_str())),
                realized,
            );
        }
    }

    /// If the CLICKABLE "Update ready" notice pill is showing and the window-px point
    /// `(x, y)` lands on it, APPLY the update (one click — the pill says "Update ready";
    /// clicking it IS the upgrade) + clear the notice, and return `true` (the click is
    /// consumed). With nothing actually staged the same call falls back to the details
    /// overlay. Reads the SAME geometry the painter uses
    /// ([`crate::notice::notice_rect`]), so the hit region matches the pixels. `false`
    /// when there is no clickable notice or the point misses it (the click flows on).
    pub(crate) fn notice_click(&mut self, wid: crate::WindowId, x: f64, y: f64) -> bool {
        let Some(n) = self.notice.as_ref().filter(|n| n.is_update_ready()) else {
            return false;
        };
        let Some(ws) = self.windows.get(&wid) else {
            return false;
        };
        let (cw, ch) = self.win_cell_size(wid);
        let pad = self.win_pad(wid) as f32;
        let top = (self.win_pad_top(wid) + self.win_head(wid)) as f32;
        let geom = crate::settings::SettingsGeom {
            cw: cw as f32,
            ch: ch as f32,
            font_px: self.win_font_px(wid),
            cols: ws.cols as usize,
            panel_rows: 0,
        };
        let (rx, ry, rw, rh) = crate::notice::notice_rect(n, &geom);
        let (x, y) = self.window_to_frame(wid, x, y);
        let (px, py) = (x as f32 - pad, y as f32 - top);
        if px >= rx && px < rx + rw && py >= ry && py < ry + rh {
            // Dismiss the pill and UPGRADE (one click; details-overlay fallback when
            // nothing is actually staged) — see `apply_update_or_details`.
            self.notice = None;
            self.apply_update_or_details();
            true
        } else {
            false
        }
    }

    /// Map a window-px point to what the Update overlay under it hits (close dot / Close /
    /// Check / Install). `None` when the overlay is closed on `wid` or the point misses.
    pub(crate) fn update_hit_at(&self, wid: crate::WindowId, x: f64, y: f64) -> Option<UpdateHit> {
        let ws = self.windows.get(&wid)?;
        let u = ws.update_screen()?;
        let panel_rows = ws.overlay_rows();
        if panel_rows == 0 {
            return None;
        }
        let (cw, ch) = self.win_cell_size(wid);
        let pad = self.win_pad(wid) as f32;
        let top = (self.win_pad_top(wid) + self.win_head(wid)) as f32;
        let geom = crate::settings::SettingsGeom {
            cw: cw as f32,
            ch: ch as f32,
            font_px: self.win_font_px(wid),
            cols: ws.cols as usize,
            panel_rows,
        };
        let (x, y) = self.window_to_frame(wid, x, y);
        crate::update_screen::update_hit(u, &geom, x as f32 - pad, y as f32 - top)
    }

    /// Resolve a click on the Update overlay to its action. Close dot / Close → close;
    /// Check → a fresh check; Install → apply the staged build (re-exec).
    pub(crate) fn update_screen_click(&mut self, wid: crate::WindowId, hit: UpdateHit) {
        match hit {
            UpdateHit::Close => self.update_screen_exit(wid),
            UpdateHit::Check => self.update_screen_check(),
            UpdateHit::Install => {
                if let Some(proxy) = &self.proxy {
                    let _ = proxy.send_event(crate::Wake::ApplyStagedUpdate);
                }
            }
        }
    }

    /// While the Update overlay is open on `wid`, SWALLOW every key (return `true`): `Esc`
    /// closes; `Enter` triggers the button painted as the highlighted DEFAULT in each
    /// state — Install when a build is staged, else Check for Updates while checks are
    /// enabled, else Close. (The button row highlights Install when staged, Check when
    /// up-to-date + enabled, and only Close when checks are disabled, so Return always
    /// fires the option the user sees as the default.) Closed ⇒ `false` (keys flow
    /// normally). Mirrors `on_key_about_mode`.
    #[cfg(test)]
    pub(crate) fn on_key_update_mode(
        &mut self,
        wid: crate::WindowId,
        ev: &winit::event::KeyEvent,
    ) -> bool {
        use winit::keyboard::{Key, NamedKey};
        let Some(default) = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.update_screen())
            .map(|u| u.default_action())
        else {
            return false;
        };
        match &ev.logical_key {
            Key::Named(NamedKey::Escape) => self.update_screen_exit(wid),
            // Return fires the button painted as the highlighted default in this state.
            Key::Named(NamedKey::Enter) => self.update_screen_click(wid, default),
            _ => {}
        }
        true
    }

    /// The ENGINE-NEUTRAL twin of [`Self::on_key_update_mode`] — reached by controller
    /// `key`/`text` verbs. The caller still swallows the event from the PTY.
    #[cfg(test)]
    pub(crate) fn update_input_event(
        &mut self,
        wid: crate::WindowId,
        ev: &crate::input::InputEvent,
    ) {
        use crate::input::InputEvent;
        use aterm_types::keyboard::{Key as TKey, KeyEventType, NamedKey as TNamed};
        let Some(default) = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.update_screen())
            .map(|u| u.default_action())
        else {
            return;
        };
        if let InputEvent::Key {
            key, event_type, ..
        } = ev
            && !matches!(event_type, KeyEventType::Release)
        {
            match key {
                TKey::Named(TNamed::Escape) => self.update_screen_exit(wid),
                TKey::Named(TNamed::Enter) => self.update_screen_click(wid, default),
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::App;
    use crate::WindowId;
    use crate::native_app::{AppViewState, UpdateOutcome};
    use crate::native_updater_service::CheckStart;

    /// ONE-CLICK fallback (MenuAction::ApplyUpdate with nothing staged): the unit-test
    /// environment has no strictly-newer build in the update ledger, so the one-click
    /// affordance must open the native Settings DETAILS route — an honest surface — rather than
    /// silently doing nothing or blindly re-exec'ing the process. (The staged branch —
    /// a real apply — is exercised by the release-flow E2E, not unit tests: exec never
    /// returns.)
    #[test]
    fn apply_update_or_details_falls_back_to_the_native_settings_route() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(
            !app.staged_update_ready(),
            "test env must not have a strictly-newer staged build"
        );
        app.apply_update_or_details();
        let (_, view) = app.active_native_view(wid).expect("native Settings tab");
        assert!(matches!(
            app.native_runtime.view_state(view),
            Some(crate::native_app::AppViewState::Settings(state))
                if state.route == crate::native_settings::SettingsRoute::SoftwareUpdate
        ));
        // Repeating the affordance focuses the same route; it does not create a
        // duplicate tab or toggle a modal over the app.
        let tabs = app.windows.get(&wid).unwrap().tab_set.len();
        app.apply_update_or_details();
        assert_eq!(app.windows.get(&wid).unwrap().tab_set.len(), tabs);
        assert!(app.windows.get(&wid).unwrap().update_screen().is_none());
    }

    #[test]
    fn selectable_update_menu_click_surfaces_dirty_native_preflight_block() {
        let dir =
            std::env::temp_dir().join(format!("aterm-update-menu-dirty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("draft.md");
        std::fs::write(&path, "draft\n").unwrap();
        let uri = format!("file://{}", path.to_string_lossy().replace(' ', "%20"));

        let mut app = App::headless_for_test();
        app.open_document_tab(crate::native_app::AppKind::Editor, &uri)
            .unwrap();
        let wid = WindowId(0);
        app.dispatch_native_event(
            wid,
            crate::native_app::AppEvent::TextInput(crate::native_app::TextInputEvent::Commit(
                "unsaved ".to_string(),
            )),
        )
        .unwrap();

        // These are the exact observed facts that the production ApplyUpdate menu
        // method passes after seeing the QA same-binary stage. No global env mutation
        // and no process replacement can occur: dirty-state preflight must win first.
        let outcome = app
            .apply_update_or_details_with_facts(true, true)
            .expect("staged click produces an apply outcome");
        assert!(matches!(
            &outcome,
            UpdateOutcome::Blocked { reasons }
                if reasons.iter().any(|reason| reason.contains("Checkpoint Drafts"))
        ));
        let (_, view) = app
            .active_native_view(wid)
            .expect("Software Update details tab");
        assert!(matches!(
            app.native_runtime.view_state(view),
            Some(AppViewState::Settings(state))
                if state.route == crate::native_settings::SettingsRoute::SoftwareUpdate
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn menu_check_route_joins_one_process_owned_check() {
        let mut app = App::headless_for_test();
        let ticket = match app.native_updater_service.request_check() {
            CheckStart::Start(ticket) => ticket,
            other => panic!("expected seeded in-flight check, got {other:?}"),
        };

        assert_eq!(
            app.open_software_update_route_and_check(),
            Ok(UpdateOutcome::Accepted)
        );
        assert_eq!(app.native_updater_service.snapshot().active, Some(ticket));
        assert_eq!(app.native_updater_service.snapshot().generation, 1);
        assert_eq!(
            app.open_software_update_route_and_check(),
            Ok(UpdateOutcome::Accepted)
        );
        assert_eq!(app.native_updater_service.snapshot().active, Some(ticket));
        assert_eq!(app.native_updater_service.snapshot().generation, 1);

        let (_, view) = app
            .active_native_view(WindowId(0))
            .expect("Settings update route");
        assert!(matches!(
            app.native_runtime.view_state(view),
            Some(AppViewState::Settings(state))
                if state.route == crate::native_settings::SettingsRoute::SoftwareUpdate
        ));
    }
}
