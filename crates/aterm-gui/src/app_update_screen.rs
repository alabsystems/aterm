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

/// Serializes the tests that both WRITE and READ the process-wide scratch update
/// ledger.
///
/// `App::headless_for_test` points `ATERM_UPDATE_ROOT` at ONE scratch root per test
/// process (so the unit suite cannot write the developer's real `health.toml`), and
/// the ledger's standing slots — `last_apply_failure_target_build` above all — are
/// single-valued. A test that records a failure for its own build number and then
/// asks the reducer to read it back is therefore racing every sibling that records
/// one for a different build: observed, not theorised, on the first parallel run.
/// The build numbers below are already unique per test, which keeps the COUNTS
/// independent; this keeps the standing slots independent too.
///
/// Poison is absorbed deliberately: a panicking test has already failed, and letting
/// it convert every later ledger test into a second failure only hides the first.
#[cfg(test)]
pub(crate) static UPDATE_LEDGER_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Take [`UPDATE_LEDGER_TEST_LOCK`] for the rest of the calling test.
#[cfg(test)]
pub(crate) fn hold_update_ledger_for_test() -> std::sync::MutexGuard<'static, ()> {
    UPDATE_LEDGER_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// `true` while the `ATERM_DEBUG_SEAMLESS_REEXEC` QA seam is armed — and it SAYS
/// SO, once per process, the first time anything asks.
///
/// The seam re-execs the SAME binary through the full seamless handoff + adopt
/// path so the shell-survives-an-update contract is testable without cutting a
/// release. That is worth keeping. What is not worth keeping is a release binary
/// that reports "update ready" and takes the handoff route because of an ambient
/// variable, with nothing on the record saying which of the two it did. It no
/// longer relaxes the adoption identity gate either — see
/// `seamless::normalize_commit`, where the `unknown`-commit admission is now
/// `cfg!(debug_assertions)` alone.
pub(crate) fn debug_seamless_reexec_armed() -> bool {
    static ARMED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ARMED.get_or_init(|| {
        // THE ENVIRONMENT ANSWERS THIS, and nothing else can. Calling this
        // function here — as the "make the knobs loud" refactor did — is a
        // self-recursive `OnceLock`: the initializer waits on the `Once` it is
        // itself running, so the FIRST caller parks forever. It is reached from
        // `apply_staged_update_now`, i.e. the main thread, on every apply.
        let on = std::env::var_os("ATERM_DEBUG_SEAMLESS_REEXEC").is_some();
        if on {
            aterm_log::warn!(
                "$ATERM_DEBUG_SEAMLESS_REEXEC is set: this process reports a staged \
                 update as READY and applies it by re-execing THIS SAME BINARY. No \
                 bundle is verified, staged or swapped — it is the handoff QA seam, \
                 not an update. Unset it to restore normal update behaviour."
            );
        }
        on
    })
}

impl App {
    /// Snapshot the current process-owned updater reducer into a fresh [`UpdateState`].
    /// This is memory-only: ledger and installed-bundle facts enter the reducer solely
    /// through typed worker completions, so Settings/introspection never block input.
    /// `checking` marks a manual check as in flight.
    pub(crate) fn update_snapshot(&self, checking: bool) -> UpdateState {
        let snapshot = self.native_updater_service.snapshot();
        // The retry disposition is EVENT-LOOP state, so only the host can answer it —
        // and the answer is half of what a person needs from a failed apply ("wait" vs
        // "press this").
        let retry = self.apply_retry_for(snapshot.staged.as_ref().map(|staged| staged.build));
        UpdateState::from_service(snapshot, checking, retry)
    }

    /// Whether the automatic apply lane still INTENDS this exact staged build — the
    /// difference between "leave it alone and it will land" and "this will not move
    /// until you ask for it".
    ///
    /// ASK EVERY CARRIER OF THE ANSWER, NOT ONE. There are two — a live
    /// `auto_apply_intent` (a failure does not necessarily consume it; a
    /// control-request apply, for one, leaves it armed) and an `AutoApplyManualOnly`
    /// latch that still carries a lapse deadline — and either one means the loop will
    /// come back to this artifact.
    ///
    /// THEY DO NOT COME BACK THE SAME WAY, which is why the wording this feeds says
    /// "by itself" and not "a retry is armed". Only the intent's `retry_at` is folded
    /// into the loop's own deadline (`fold_auto_apply_deadline`); the latch is
    /// released by `lapse_expired_auto_apply_manual_only` at the top of
    /// `arm_native_auto_apply`, so on a genuinely idle machine it waits for the next
    /// background check rather than for its own deadline. Both do land unaided; only
    /// one of them is armed to the second. Both must name THIS build, because a leftover for
    /// a superseded artifact schedules nothing for the one on screen, and automatic
    /// apply has to actually be enabled in config or a lapse would only re-arm into a
    /// poll that answers `Clear`.
    ///
    /// Extracted from the failure-pill decision in
    /// [`Self::react_to_update_apply_outcome`] so the STANDING surfaces (the Version
    /// menu, the palette's Version row, the Settings page) answer the question the
    /// same way the transient pill does. Two answers to "will this retry?" in one
    /// program is how a user ends up waiting on something that stopped.
    pub(crate) fn automatic_apply_retry_scheduled(&self, staged_build: u64) -> bool {
        crate::app_config::update_auto_apply(&self.config)
            && (self
                .auto_apply_intent
                .is_some_and(|intent| intent.build == staged_build)
                || self.auto_apply_manual_only.is_some_and(|manual| {
                    manual.build == staged_build && manual.retry_at.is_some()
                }))
    }

    /// [`Self::automatic_apply_retry_scheduled`] as the typed value the wording law
    /// reads. `None` — no build on offer — answers `ManualOnly`, the conservative
    /// half: promising a retry that is not scheduled is the one error that costs the
    /// reader real time.
    pub(crate) fn apply_retry_for(
        &self,
        staged_build: Option<u64>,
    ) -> crate::update_apply_trouble::ApplyRetry {
        use crate::update_apply_trouble::ApplyRetry;
        match staged_build {
            Some(build) if self.automatic_apply_retry_scheduled(build) => ApplyRetry::Scheduled,
            _ => ApplyRetry::ManualOnly,
        }
    }

    /// The apply lane's standing trouble for the build a surface is offering, or
    /// `None` when there is none to report.
    ///
    /// Memory-only, like every other read a repaint may take: the failure COUNT and
    /// the failure REASON both arrive through the updater reducer's typed worker
    /// completions ([`crate::native_updater_service::UpdaterSnapshot`]), and the
    /// retry disposition is event-loop state. Nothing here touches the ledger.
    ///
    /// `staged_build` is passed in rather than read from the snapshot so the caller's
    /// own notion of "the build being offered" (the `relaunch` nudge drives the menu
    /// and the palette; the reducer's stage drives Settings) is the one the trouble
    /// describes. A streak belonging to a superseded artifact must not decorate a
    /// fresh offer.
    ///
    /// THE LEDGER HAS TO NAME THE ARTIFACT, and that is why it now does. The apply
    /// streak `failing_applies` carries is expiry-bound to the RUNNING build — it
    /// means "this machine cannot be moved through the lane" — so staging a NEWER
    /// artifact resets nothing at all: stage X failing twice, superseded by a fresh
    /// download Y, left every surface telling the reader that Y had already been
    /// tried twice and had watched its successor die, while the automatic lane had
    /// just armed a clean first attempt for it. `apply_failure_build` /
    /// `apply_failures_for_target` are the artifact-scoped pair; nothing is reported
    /// unless they name the build on offer.
    pub(crate) fn apply_trouble_for(
        &self,
        staged_build: u64,
    ) -> Option<crate::update_apply_trouble::ApplyTrouble> {
        use crate::update_apply_trouble::ApplyTrouble;
        let snapshot = self.native_updater_service.snapshot();
        // Only a genuinely newer artifact is on offer at all…
        if staged_build <= snapshot.current_build {
            return None;
        }
        // …and only the artifact the failures were actually about may wear them. A
        // zero target is a ledger that predates this field, or an attempt with no
        // stage behind it (the QA seam): unknown, therefore silent.
        if snapshot.apply_failure_build != staged_build {
            return None;
        }
        ApplyTrouble::new(
            snapshot.apply_failures_for_target,
            &snapshot.apply_failure,
            self.apply_retry_for(Some(staged_build)),
        )
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
        if crate::app_update_screen::debug_seamless_reexec_armed() {
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
        let debug_seamless = crate::app_update_screen::debug_seamless_reexec_armed();
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
            // A simulated apply is still fully visible — it just does not get to
            // leave an `apply`-class failure in the durable ledger, which nothing
            // but a real successful apply can clear.
            if debug_seamless {
                self.react_to_update_apply_outcome("manual", outcome.clone(), true);
            } else {
                self.surface_update_apply_outcome("manual", outcome.clone(), true);
            }
            Some(outcome)
        } else {
            let _ = self.open_settings_tab(crate::native_settings::SettingsRoute::SoftwareUpdate);
            None
        }
    }

    /// Make every returned updater outcome observable. A manual click opens the native
    /// Software Update route on failure/block so the enabled menu item can never look
    /// inert; automatic/control paths retain the non-disruptive notification + log.
    ///
    /// The durable ledger is written FIRST, then the UI reacts. A simulated apply takes
    /// the other door ([`Self::react_to_update_apply_outcome`]) so it stays visible
    /// without leaving a real failure behind it.
    pub(crate) fn surface_update_apply_outcome(
        &mut self,
        source: &str,
        outcome: crate::native_app::UpdateOutcome,
        open_details: bool,
    ) {
        self.record_apply_outcome_in_ledger(&outcome);
        self.react_to_update_apply_outcome(source, outcome, open_details);
    }

    /// Persist the apply-lane verdict into the updater's own health ledger and
    /// status file. Until this existed the apply lane was invisible to
    /// `aterm-ctl update status`: a handoff could fail every single time for three
    /// releases while `health.toml` stayed all-zero and status said "up to date",
    /// because only the download lane was ever recorded.
    ///
    /// `Deferred`/`Blocked` mean "not yet, conditions were not met" — normal and
    /// self-correcting, so they must NOT touch the failure streak: doing that
    /// would manufacture an escalation every time the user happened to be typing.
    ///
    /// But silent was the wrong other extreme, and it is what the owner actually
    /// hit: a staged build sat unapplied across two releases while `update
    /// status` reported `failing=0 failing_applies=0` and advised a relaunch,
    /// because the refusal reached this function and stopped here. "Nothing is
    /// wrong" and "we declined, here is why" are different answers and the file
    /// could only say the first. `record_apply_refusal` is the separate,
    /// non-streak slot for the second; it is expiry-bound to the RUNNING build,
    /// since a successful in-session apply execs away and never returns to clear
    /// it. Which outcome is which — and why nothing here can record a SUCCESS —
    /// is [`apply_ledger_verdict`].
    ///
    /// A FAILURE ALSO REPUBLISHES, because the write is the only moment the window
    /// can learn about it in time — see the comment on the `Failed` arm.
    fn record_apply_outcome_in_ledger(&mut self, outcome: &crate::native_app::UpdateOutcome) {
        let snapshot = self.native_updater_service.snapshot();
        let current_build = snapshot.current_build;
        // WHICH ARTIFACT THE ATTEMPT WAS FOR. Every failure path re-arms the exact
        // stage before returning here (`abort_apply` leaves `staged` in place), so
        // the reducer's stage IS the target. `0` — the debug seam, or a stage already
        // consumed — is recorded honestly as "unknown", and a surface that cannot
        // match the target simply reports no trouble.
        let target_build = snapshot.staged.as_ref().map_or(0, |staged| staged.build);
        match apply_ledger_verdict(outcome) {
            ApplyLedgerVerdict::Failed(message) => {
                // FEED THE REDUCER AT THE WRITE. The facts that used to carry a
                // failure back to the window are gathered by a worker that has
                // already run by the time this executes, so the FIRST failed apply
                // — the case this surface exists for — reached the Version menu and
                // the palette only when some later, unrelated reconcile landed.
                // `record_apply_failure` hands back what it just wrote, under the
                // same lock, so the surfaces move now.
                if let Some(recorded) =
                    aterm_update::record_apply_failure(current_build, target_build, &message)
                    && self.native_updater_service.note_apply_failure(&recorded)
                {
                    self.publish_native_update_state();
                }
            }
            ApplyLedgerVerdict::Refused(reason) => {
                aterm_update::record_apply_refusal(current_build, &reason);
            }
            ApplyLedgerVerdict::Silent => {}
        }
    }

    /// React to an apply outcome in the UI and the log WITHOUT writing the durable
    /// health ledger.
    ///
    /// THE QA SEAM ENTERS HERE, AND THAT IS THE WHOLE POINT. Under
    /// `ATERM_DEBUG_SEAMLESS_REEXEC` the handoff runs with no
    /// `ApplyAttemptTicket` (`start_native_update_handoff` refuses a `None` ticket
    /// in every other case), so a failed debug handoff describes a SIMULATED apply
    /// of the running binary — not a staged build that could not be made to run.
    /// Routing it through [`Self::surface_update_apply_outcome`] recorded it as a
    /// genuine `apply`-class failure, and that streak is cleared only by a
    /// successful apply ([`Health::record_apply_success`]) — never by a healthy
    /// check. So QA runs accumulated permanently and drove the machine to
    /// `is_persistent()`, raising the "aterm auto-update is failing" notification
    /// on a machine whose apply lane had never actually been asked to do anything.
    /// Observed in the field: 497 counted apply failures, every one of them a
    /// `"debug overlap handoff failed safely"` from this seam.
    pub(crate) fn react_to_update_apply_outcome(
        &mut self,
        source: &str,
        outcome: crate::native_app::UpdateOutcome,
        open_details: bool,
    ) {
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
                } else if let Some(staged_build) = self
                    .native_updater_service
                    .snapshot()
                    .staged
                    .as_ref()
                    .map(|staged| staged.build)
                {
                    // "Update paused — manual retry" named a mechanism, not an
                    // action: there is no "retry" control anywhere in the UI, so a
                    // user who read it had nothing to click. Worse, it was flatly
                    // wrong half the time — a physical handoff failure inside its
                    // budget schedules another automatic attempt, and this arm
                    // claimed the automatic lane had given up.
                    //
                    // WHICH ANSWER IS TRUE IS A QUESTION ABOUT SCHEDULING STATE,
                    // so ask every carrier of it rather than one. There are two —
                    // a live `auto_apply_intent` (this failure did not consume it;
                    // a control-request apply, for one, leaves it armed) and a
                    // manual-only latch carrying a lapse deadline — and either
                    // means a wake is already folded into the event loop. Both
                    // must name THIS staged artifact, because a leftover for a
                    // superseded build schedules nothing for the one on screen,
                    // and automatic apply must actually be enabled or the lapse
                    // would re-arm into a poll that answers `Clear`.
                    let retry_scheduled = self.automatic_apply_retry_scheduled(staged_build);
                    if retry_scheduled {
                        // …AND A SCHEDULED RETRY IS NOT AUTOMATICALLY WORTH A PILL.
                        // The physical lane may spend nine attempts before it gives
                        // up, so painting this on every one of them is a pill every
                        // ~40 minutes for most of a day — describing a state whose
                        // only honest advice is "wait", which the user is already
                        // doing. Owner instruction: do not notify on a schedule for
                        // a failure that is not going to fix itself. So the first
                        // failure for an artifact speaks and the rest are quiet;
                        // the log line above still records every one, and the
                        // moment the lane genuinely runs out the `else` below fires
                        // once with a control the user can press.
                        if self.physical_failure_deserves_a_pill(staged_build) {
                            self.surface_nonmodal_update_status(
                                "↑ Update delayed — retries on its own",
                            );
                        }
                    } else {
                        self.surface_nonmodal_update_status("↑ Update paused — see Version menu");
                    }
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

    /// The transient card for a FAILED USER GESTURE (see
    /// [`crate::notice::TransientNotice::gesture_failure`]): every call site
    /// pairs this with the stderr line that carries the full error — the card
    /// answers the person, the log answers the investigator.
    /// [`Self::surface_nonmodal_update_status`] with an explicit lifetime — for
    /// the handoff cards, which must outlive the freeze they explain rather than
    /// fading out in the middle of it.
    pub(crate) fn surface_update_status_for(&mut self, text: &str, ttl: std::time::Duration) {
        self.notice = Some(crate::notice::TransientNotice::update_status_for(
            text,
            ttl,
            std::time::Instant::now(),
        ));
        self.request_redraw_all_windows();
    }

    pub(crate) fn surface_gesture_failure(&mut self, text: &str) {
        self.notice = Some(crate::notice::TransientNotice::gesture_failure(
            text,
            std::time::Instant::now(),
        ));
        self.request_redraw_all_windows();
    }

    /// Whether the celebration card's pixels are actually MOVING right now — sparkles
    /// enabled and motion live. The notice cannot know either: one is config, the
    /// other is the reduced-motion amplitude. Both must be true before the card earns
    /// a per-frame wake and a re-raster (2026-08-20 round-12 audit).
    pub(crate) fn notice_is_sparkling(&self) -> bool {
        self.config.notice_sparkle_or_default()
            && self
                .motion_policy(true)
                .amplitude(crate::motion::MotionEffect::NoticePill)
                > 0.0
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
            // The always-visible offer carries the apply lane's verdict on ITSELF. A
            // Version menu that says "restart now" while the engine has already tried
            // and failed is the surface that made the 2026-08-21 field state
            // unreadable; the row now says how many attempts died, why, and whether
            // anything is still scheduled.
            let trouble = staged
                .as_ref()
                .and_then(|(build, _)| self.apply_trouble_for(*build));
            crate::menu::update_version_menu(
                handle,
                staged.as_ref().map(|(b, v)| (*b, v.as_str())),
                trouble.as_ref(),
                realized,
            );
        }
    }

    /// Route a left press that landed on the notice card. A press on the CLICKABLE
    /// "Update ready" card APPLIES the update (one click — the card says "Update ready";
    /// clicking it IS the upgrade); a press on any OTHER card dismisses it. Either way the
    /// card goes away and the click is CONSUMED (`true`). With nothing actually staged the
    /// apply falls back to the details overlay.
    ///
    /// Two gates keep this from firing on a card the user cannot see:
    ///
    /// * The card must be ON GLASS IN THIS WINDOW
    ///   ([`crate::WindowState::notice_is_on_glass`]). `self.notice` is App-global and the
    ///   paint-only cards share ONE composited slot, so without this a window whose card
    ///   was suppressed — serious mode, a zero-column window, a rejected paint rect — or
    ///   one where the level-up flourish still owns the slot, hit-tested a rectangle of
    ///   empty screen, and a click there silently re-execed the app.
    /// * The card must still be legible ([`crate::notice::CLICK_MIN_ALPHA`]). The exit
    ///   tail runs to nothing, and a target you cannot see is a trap.
    ///
    /// DISMISSING A NON-ACTIONABLE CARD IS NOT A COURTESY, IT IS A ROUTING FIX. This runs
    /// BEFORE the tab-strip and terminal mouse paths, so a press the card visually caught
    /// used to fall straight through a status card into whatever was underneath — with an
    /// in-grid tab strip under the old placement, that could CLOSE A TAB.
    ///
    /// Reads the SAME geometry the painter uses ([`crate::notice::notice_rect`]) — same
    /// `now`, same motion amplitude, same reserved chrome rows — so the hit region is the
    /// pixels. `false` when there is no visible card or the point misses it (the click
    /// flows on).
    pub(crate) fn notice_click(&mut self, wid: crate::WindowId, x: f64, y: f64) -> bool {
        let Some(n) = self.notice.as_ref() else {
            return false;
        };
        let Some(ws) = self.windows.get(&wid) else {
            return false;
        };
        if !ws.notice_is_on_glass() {
            return false;
        }
        let now = std::time::Instant::now();
        if n.alpha(now) < crate::notice::CLICK_MIN_ALPHA {
            return false;
        }
        let actionable = n.is_update_ready();
        let admin_names = n.admin_step_names().map(<[String]>::to_vec);
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
        // The SAME (now, motion, reserved chrome) the painter used, so the hit region
        // tracks the card through its slide instead of lagging behind the pixels.
        let motion = self
            .motion_policy(true)
            .amplitude(crate::motion::MotionEffect::NoticePill);
        let (x, y) = self.window_to_frame(wid, x, y);
        let (px, py) = (x as f32 - pad, y as f32 - top);
        let Some(hit) =
            crate::notice::notice_hit(n, &geom, now, motion, self.notice_clear_rows(), px, py)
        else {
            return false;
        };
        self.notice = None;
        match (admin_names, hit) {
            // THE ADMIN CARD'S INSTALL: the osascript door, through the very seam the
            // Packages page's buttons use (`execute_native_packages`, busy-gated, one
            // worker). macOS raises its own dialog; nothing here sees a password. A
            // refusal (inert manager, a verb already running) is answered on a plain
            // status card so the press is never silently swallowed.
            (Some(names), crate::notice::NoticeHit::Install) => {
                let outcome = self.execute_native_packages(
                    crate::native_app::PackagesRequest::InstallElevated { names },
                );
                match outcome {
                    crate::native_app::PackagesOutcome::Accepted => {
                        self.surface_nonmodal_update_status(
                            "\u{21e3} Installing through macOS \u{2014} progress in Settings \u{25b8} Packages",
                        );
                    }
                    crate::native_app::PackagesOutcome::Blocked { message }
                    | crate::native_app::PackagesOutcome::Failed { message } => {
                        aterm_log::warn!("admin install did not start: {message}");
                        self.surface_gesture_failure(&format!(
                            "Admin install did not start \u{2014} {message}"
                        ));
                    }
                }
            }
            // NOT NOW: record the exact set so the card is not raised again until the
            // set changes (`packages_screen::admin_step_should_show`). The write is a
            // tiny file, but it is still a disk write, so it leaves the UI thread; a
            // failure only means the card comes back next pass.
            (Some(names), crate::notice::NoticeHit::NotNow) => {
                let config_path = crate::app_config::config_path();
                let spawned = std::thread::Builder::new()
                    .name("aterm-admin-step-dismiss".into())
                    .spawn(move || {
                        if let Err(error) = crate::packages_screen::record_admin_step_dismissal(
                            config_path.as_deref(),
                            &names,
                        ) {
                            aterm_log::warn!("admin step dismissal not recorded: {error}");
                        }
                    });
                if let Err(error) = spawned {
                    aterm_log::warn!("admin step dismissal not recorded: {error}");
                }
            }
            // A press on the admin card's body dismisses it for now — nothing is
            // recorded, and the next pass re-raises it.
            (Some(_), crate::notice::NoticeHit::Body) => {}
            (None, _) => {
                if actionable {
                    // UPGRADE (one click; details-overlay fallback when nothing is actually
                    // staged) — see `apply_update_or_details`.
                    self.apply_update_or_details();
                }
            }
        }
        self.request_redraw_all_windows();
        true
    }

    /// The in-grid chrome rows the notice card must sit BELOW: the tab strip and the
    /// status bars own the first [`Self::chrome_rows`] rows of the terminal area, and
    /// the card is not allowed to cover chrome the user clicks (or reads). One
    /// accessor so the painter and the hit test cannot disagree about where the card is.
    pub(crate) fn notice_clear_rows(&self) -> f32 {
        f32::from(self.chrome_rows())
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

/// What one apply outcome must write to the DURABLE health ledger.
///
/// Split out of [`App::surface_update_apply_outcome`]'s UI reaction because the write
/// itself is unobservable from this crate — `aterm_update::record_apply_*` resolve
/// `HOME` and rewrite `Updates/health.toml` — so the routing is the only part a unit
/// test can hold still. The streak arithmetic behind each verdict is `aterm-update`'s
/// (`Health::record_apply_failure` / `record_apply_success`).
#[derive(Debug, PartialEq, Eq)]
enum ApplyLedgerVerdict {
    /// The apply was ATTEMPTED and the staged build did not become the running
    /// build. Advances the apply streak — the one that escalates to the
    /// "aterm auto-update is failing" notification.
    Failed(String),
    /// Refused before it could become a failure. Fills the standing-explanation
    /// slot and touches no streak: a terminal that happened to be busy must not
    /// manufacture an escalation.
    Refused(String),
    /// Nothing durable to say yet.
    Silent,
}

/// Route one apply outcome to its ledger verdict.
///
/// `Accepted` IS DELIBERATELY SILENT: it means the apply worker was started, or that
/// we joined an in-flight/`Applying` request — never that an apply completed, because
/// a successful one execs away and never returns here (`app_native.rs`: "A successful
/// replacement never returns"). Recording success on it was the bug that made the
/// ledger blind: every manual/control click WIPED the streak (and the standing refusal
/// with it) seconds before the asynchronous `Failed` arrived, so the apply streak was
/// capped at 1, `PERSISTENT_AFTER = 3` was unreachable on those lanes, and one
/// troubleshooting click could erase a background streak that had already climbed.
/// The apply lane is cleared at the ONE place success is provable — a boot sentinel
/// armed for the build that is now running, in `aterm-update`'s `confirm_boot_health`.
///
/// `InstalledNeedsRelaunch` is likewise silent, but for the opposite reason: it is
/// neither a refusal nor a pending attempt — the bytes ARE in place and the next
/// launch runs them, which the status line already reports from the staged marker.
fn apply_ledger_verdict(outcome: &UpdateOutcome) -> ApplyLedgerVerdict {
    match outcome {
        UpdateOutcome::Failed { message } => ApplyLedgerVerdict::Failed(message.clone()),
        UpdateOutcome::Blocked { reasons } => ApplyLedgerVerdict::Refused(reasons.join(" · ")),
        UpdateOutcome::Deferred { reason } => ApplyLedgerVerdict::Refused(reason.clone()),
        UpdateOutcome::Accepted | UpdateOutcome::InstalledNeedsRelaunch { .. } => {
            ApplyLedgerVerdict::Silent
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ApplyLedgerVerdict, apply_ledger_verdict, debug_seamless_reexec_armed};
    use crate::App;
    use crate::WindowId;
    use crate::native_app::{AppViewState, UpdateOutcome};
    use crate::native_updater_service::CheckStart;

    /// REGRESSION — the QA seam's own flag used to hang the process that asked
    /// for it. Its `OnceLock` initializer called the function it was
    /// initializing, so the first caller waited on the `Once` it was itself
    /// inside, forever. `apply_staged_update_now` reaches it on the MAIN
    /// THREAD, which made every update apply a permanent freeze — the exact
    /// failure the whole handoff design exists to avoid.
    ///
    /// This test deadlocks rather than fails if the shape comes back. That is
    /// the honest signal: the defect IS a hang, and a suite that stops is
    /// louder than an assertion that never runs.
    #[test]
    fn the_reexec_seam_reads_the_environment_and_never_itself() {
        let armed = debug_seamless_reexec_armed();
        assert_eq!(
            armed,
            std::env::var_os("ATERM_DEBUG_SEAMLESS_REEXEC").is_some(),
            "the seam is armed exactly when its variable is present"
        );
        assert_eq!(
            armed,
            debug_seamless_reexec_armed(),
            "and the cached answer is stable across callers"
        );
    }

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
        // The shipping encoder, not a hand-rolled `format!` — the latter is
        // malformed on Windows (drive letter + backslashes after the authority
        // slot), so this test could not even open its document there.
        let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();

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

    /// Stage one strictly-newer build through the REAL check reducer, so the
    /// snapshot the pill predicate reads is the one production reads.
    /// Stage `build` through the SHIPPING check reducer, carrying the apply-lane
    /// facts a real `durable_update_status` read would carry beside it.
    ///
    /// `apply_lane` is read from the ledger the test itself just wrote, through the
    /// same `aterm_update::apply_lane_report` the worker uses, so a check landing
    /// after a failure reproduces exactly what production hands the reducer.
    #[cfg(target_os = "macos")]
    fn stage_build_with_ledger(app: &mut App, build: u64) {
        let current_build = app.native_updater_service.snapshot().current_build;
        // THE SHIPPING READER, not a hand-built literal. `durable_update_status` is
        // the one place the check lane's status and the apply lane's ledger are read
        // side by side, and it is the only path a FRESH PROCESS has to the apply
        // reason at all — so a fixture that assembled those three fields itself would
        // leave the startup lane untested while looking thorough. Only the staged
        // artifact is substituted: the scratch root has no real download in it.
        let base = aterm_update::status(current_build)
            .map(crate::app_native::durable_update_status)
            .expect("the scratch staging root yields a status under test");
        let status = crate::native_updater_service::DurableUpdateStatus {
            enabled: true,
            staged_build: Some(build),
            staged_version: Some(format!("1.0.{build}")),
            staged_commit: Some("0123456789abcdef0123456789abcdef01234567".to_string()),
            staged_dmg_sha256: Some("ab".repeat(32)),
            changelog: None,
            outcome: "staged".to_string(),
            installable: true,
            channel_unreadable: false,
            ..base
        };
        let CheckStart::Start(ticket) = app.native_updater_service.request_check() else {
            panic!("a service between checks must start exactly one");
        };
        assert_eq!(
            app.native_updater_service.finish_check(ticket, status),
            crate::native_updater_service::CheckCompletion::Reduced,
        );
        app.publish_native_update_state();
    }

    /// An `AutoApplyIntent` for `build`, i.e. the state in which the automatic lane
    /// still means to try this artifact by itself.
    #[cfg(target_os = "macos")]
    fn arm_intent(app: &mut App, build: u64) {
        app.auto_apply_manual_only = None;
        app.auto_apply_intent = Some(crate::AutoApplyIntent {
            build,
            dmg_sha256: [0xab; 32],
            retry_at: std::time::Instant::now() + std::time::Duration::from_secs(600),
            attempts: 0,
            apply_by: std::time::Instant::now() + std::time::Duration::from_secs(600),
        });
    }

    /// THE WHOLE SHIPPING ASSEMBLY, FROM ONE REAL FAILED APPLY.
    ///
    /// Everything else about this work can be asserted through seams — `ApplyTrouble`
    /// renders in isolation, `UpdateState::with_apply_lane` attaches a reason to a
    /// fixture — and NONE of those seams is on the path a user's machine takes. This
    /// test takes that path and only that path: `App::surface_update_apply_outcome`
    /// with a real `UpdateOutcome::Failed`, the real ledger write (the harness points
    /// `ATERM_UPDATE_ROOT` at a scratch root), the real reducer, `apply_trouble_for`,
    /// `apply_retry_for`, `from_service`, and the exact label constructor
    /// `refresh_version_menu` and the palette both call.
    ///
    /// It also pins the FIRST failure, which is the one the machinery could not
    /// deliver: the ledger write is on the event loop, every path that used to carry
    /// apply facts back into the reducer is a worker observation gathered BEFORE that
    /// write, and no reconcile is run anywhere below. If the failure does not reach
    /// these surfaces at the instant it is recorded, it does not reach them at all.
    ///
    /// Ledger isolation note: the count asserted here is the ARTIFACT-scoped one, and
    /// every build number below is unique to this test, so a sibling test writing the
    /// process-wide scratch ledger in parallel cannot perturb it.
    #[cfg(target_os = "macos")]
    #[test]
    fn one_real_failed_apply_reaches_every_standing_surface_at_once() {
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = App::headless_for_test();
        let current_build = app.native_updater_service.snapshot().current_build;
        let build = current_build + 4_101;
        stage_build_with_ledger(&mut app, build);
        arm_intent(&mut app, build);

        // PRECONDITION: a clean stage says nothing about failures, anywhere.
        assert!(
            app.apply_trouble_for(build).is_none(),
            "a stage nothing has attempted carries no trouble"
        );
        assert_eq!(
            crate::menu::staged_apply_label(
                "\u{2191}",
                build,
                "9.9.9",
                app.apply_trouble_for(build).as_ref()
            ),
            "\u{2191} Update to v9.9.9 \u{2014} restart now"
        );

        // ONE REAL FAILURE, through the door the handoff completion uses. Nothing
        // else happens: no reconcile, no check, no republish the test performs.
        app.surface_update_apply_outcome(
            "automatic handoff",
            UpdateOutcome::Failed {
                message: "overlap handoff failed safely: handoff proof ended ChildDied".to_string(),
            },
            false,
        );

        // THE VERSION MENU — the affordance that read "restart now" for hours on
        // 2026-08-21 over an engine that had already tried twice.
        let trouble = app
            .apply_trouble_for(build)
            .expect("the failure that just happened is standing trouble for this build");
        assert_eq!(
            crate::menu::staged_apply_label("\u{2191}", build, "9.9.9", Some(&trouble)),
            "\u{2191} Update to v9.9.9 \u{2014} tried once, didn\u{2019}t start; \
             retrying on its own"
        );

        // THE PALETTE ROW, gathered by `palette_live` exactly as the real repaint
        // does — including `relaunch`, which only exists because the ledger write
        // republished the update state.
        let live = app.palette_live();
        assert_eq!(
            live.staged.as_ref().map(|(b, _)| *b),
            Some(build),
            "the ledger write republished the staged nudge"
        );
        assert_eq!(live.staged_trouble.as_ref(), Some(&trouble));

        // THE SETTINGS PAGE, through `from_service` + `apply_retry_for`.
        //
        // The ESCALATION streak is process-wide in the shared scratch ledger (every
        // headless test in this binary writes the same root), so the two things it
        // decides — the louder headline, and the pointer at the log — are read from
        // the projection rather than assumed. Everything the ARTIFACT owns is exact:
        // it is keyed by a build number unique to this test.
        let projection = app.update_snapshot(false).projection();
        assert_ne!(
            projection.headline, "Update ready",
            "a build the engine has already failed to start is not simply ready"
        );
        assert_eq!(
            projection.headline,
            if projection.apply_is_failing {
                "Update ready, but it keeps failing to apply."
            } else {
                "Update ready, but applying it failed."
            },
            "one failure is already below `PERSISTENT_AFTER` and already news"
        );
        let log_pointer = if projection.apply_is_failing {
            " See aterm.log."
        } else {
            ""
        };
        assert_eq!(
            projection.detail.as_deref(),
            Some(
                format!(
                    "Version 1.0.{build} \u{b7} build {build} \u{b7} aterm tried to update once \
                     and the new version did not finish starting. It will try again by \
                     itself.{log_pointer}"
                )
                .as_str()
            )
        );
        assert_eq!(
            crate::native_settings::compact_update_detail(&projection),
            "Tried once, didn\u{2019}t start \u{b7} retrying."
        );
        assert_eq!(
            crate::native_settings::compact_update_headline(&projection),
            "Not applying"
        );

        // AND NOT ONE OF THEM LEAKS THE LEDGER'S OWN PROSE. "overlap handoff failed
        // safely" in a menu item is worse than the plain "Update ready" it replaced:
        // it tells the reader the failure succeeded.
        for surfaced in [
            crate::menu::staged_apply_label("\u{2191}", build, "9.9.9", Some(&trouble)),
            projection.detail.clone().expect("detail"),
            crate::native_settings::compact_update_detail(&projection),
            crate::native_settings::compact_update_detail_minimum(&projection),
        ] {
            for leak in ["failed safely", "handoff", "ChildDied"] {
                assert!(
                    !surfaced.contains(leak),
                    "{leak:?} leaked into {surfaced:?}"
                );
            }
        }

        // THE OTHER HALF OF THE ANSWER IS EVENT-LOOP STATE, and it has to reach the
        // words. Same ledger, same count, same cause — only the scheduling changes.
        app.auto_apply_intent = None;
        app.auto_apply_manual_only = None;
        let latched = app
            .apply_trouble_for(build)
            .expect("still standing trouble");
        assert_eq!(
            crate::menu::staged_apply_label("\u{2191}", build, "9.9.9", Some(&latched)),
            "\u{2191} Update to v9.9.9 \u{2014} tried once, didn\u{2019}t start; \
             restart now to retry"
        );
        assert!(
            app.update_snapshot(false)
                .projection()
                .detail
                .expect("detail")
                .contains("not try again until you ask"),
            "a lane that has stopped must say so on the page too"
        );
    }

    /// A FRESH PROCESS READS THE REASON OUT OF THE LEDGER, NOT ONLY THE COUNT.
    ///
    /// The other half of the wiring. `note_apply_failure` covers the failure that
    /// happens WHILE the window is open; this covers the one that happened before it
    /// opened — which is the field state exactly (the owner restarted into a build
    /// whose ledger already carried two dead attempts). The only path to the reason
    /// there is `app_native::durable_update_status`, which reads
    /// `aterm_update::apply_lane_report` beside the check lane's status, and nothing
    /// else in this crate reads it at all: without this test, deleting that read
    /// leaves every surface saying "it did not finish applying" and the whole suite
    /// green.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_check_that_lands_on_a_ledger_reads_the_reason_and_not_only_the_count() {
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = App::headless_for_test();
        let current_build = app.native_updater_service.snapshot().current_build;
        let staged = current_build + 6_301;

        // What a previous run left on disk.
        let recorded = aterm_update::record_apply_failure(
            current_build,
            staged,
            "overlap handoff failed safely: handoff proof ended AdoptionMismatch",
        )
        .expect("the scratch staging root is resolvable under test");
        assert_eq!(recorded.failures_for_target, 1);

        // This process learns all of it from the check that imports the stage —
        // nothing was recorded in THIS App.
        stage_build_with_ledger(&mut app, staged);
        app.auto_apply_intent = None;
        app.auto_apply_manual_only = None;

        let trouble = app
            .apply_trouble_for(staged)
            .expect("the ledger remembers what this process never saw");
        assert_eq!(
            crate::menu::staged_apply_label("\u{2191}", staged, "9.9.9", Some(&trouble)),
            "\u{2191} Update to v9.9.9 \u{2014} tried once, didn\u{2019}t take over; \
             restart now to retry",
            "the CAUSE came off the disk, and so did the fact that nothing is scheduled"
        );
        assert!(
            app.update_snapshot(false)
                .projection()
                .detail
                .expect("detail")
                .contains("did not take over the open sessions"),
            "…and the page says the same thing at its own width"
        );
    }

    /// A SUPERSEDED STAGE DOES NOT INHERIT THE FAILURES OF THE ONE IT REPLACED.
    ///
    /// The apply streak `failing_applies` carries expires only when the RUNNING build
    /// changes, so staging a newer artifact resets nothing at all. Before the ledger
    /// learned to name the artifact, this exact sequence — an artifact fails, a check
    /// imports a newer one — left the Version menu telling the reader that the FRESH
    /// download had already been tried and had watched its successor die, while
    /// `arm_native_auto_apply` had just armed a clean first attempt for it.
    ///
    /// Both halves run through the shipping path: `aterm_update::record_apply_failure`
    /// writes the real ledger, `NativeUpdaterService::finish_check` imports the newer
    /// stage carrying that ledger forward exactly as `durable_update_status` reads it,
    /// and `apply_trouble_for` is asked the question the menu asks.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_newer_stage_starts_clean_even_though_the_running_build_has_not_changed() {
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = App::headless_for_test();
        let current_build = app.native_updater_service.snapshot().current_build;
        let superseded = current_build + 5_201;
        let fresh = superseded + 1;

        // The artifact that actually failed, recorded exactly as a returned handoff
        // records it.
        let recorded = aterm_update::record_apply_failure(
            current_build,
            superseded,
            "overlap handoff failed safely: handoff proof ended ChildDied",
        )
        .expect("the scratch staging root is resolvable under test");
        assert_eq!(recorded.failures_for_target, 1);
        assert!(app.native_updater_service.note_apply_failure(&recorded));
        assert!(
            app.apply_trouble_for(superseded).is_some(),
            "PRECONDITION: the artifact that failed wears its own failure"
        );

        // THE CHECK LANE IMPORTS A NEWER ARTIFACT, carrying the ledger forward
        // verbatim — a stage does not touch the apply streak, by design.
        stage_build_with_ledger(&mut app, fresh);
        arm_intent(&mut app, fresh);
        assert!(
            app.native_updater_service.snapshot().failing_applies > 0,
            "PRECONDITION: the escalation streak really does survive the new stage — \
             that is what made this a bug and not a hypothetical"
        );
        assert_eq!(
            app.apply_trouble_for(fresh),
            None,
            "a build that has never been attempted must not wear the previous one's \
             attempts"
        );
        assert_eq!(
            crate::menu::staged_apply_label(
                "\u{2191}",
                fresh,
                "9.9.9",
                app.apply_trouble_for(fresh).as_ref()
            ),
            "\u{2191} Update to v9.9.9 \u{2014} restart now"
        );
        assert!(
            app.update_snapshot(false)
                .projection()
                .apply_trouble
                .is_none(),
            "…and the page agrees with the menu"
        );

        // …and the fresh artifact's OWN first failure counts from one, not from two,
        // and carries ITS cause rather than the previous artifact's.
        app.surface_update_apply_outcome(
            "automatic handoff",
            UpdateOutcome::Failed {
                message: "overlap handoff failed safely: handoff proof ended TimedOut".to_string(),
            },
            false,
        );
        let trouble = app.apply_trouble_for(fresh).expect("its own failure");
        assert_eq!(
            crate::menu::staged_apply_label("\u{2191}", fresh, "9.9.9", Some(&trouble)),
            "\u{2191} Update to v9.9.9 \u{2014} tried once, too slow to start; \
             retrying on its own",
            "the count restarted with the artifact, and the CAUSE moved with it"
        );
    }

    /// A STAGE THAT IS NOT NEWER THAN THE RUNNING BUILD IS NOT AN OFFER.
    ///
    /// `aterm-update`'s own boot-trial recovery records failures whose target IS the
    /// running build (`install.rs`), so a ledger in which the target matches the
    /// running build is reachable in production — and `apply_trouble_for` must still
    /// refuse to decorate it, because there is nothing to restart into.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_failure_recorded_against_the_running_build_decorates_nothing() {
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = App::headless_for_test();
        let current_build = app.native_updater_service.snapshot().current_build;
        let recorded = aterm_update::record_apply_failure(
            current_build,
            current_build,
            "trial recovery proof failed 3x (receipt missing); disarmed the boot sentinel \
             to keep updates possible",
        )
        .expect("the scratch staging root is resolvable under test");
        assert_eq!(recorded.target_build, current_build);
        assert!(app.native_updater_service.note_apply_failure(&recorded));

        assert_eq!(
            app.apply_trouble_for(current_build),
            None,
            "there is no newer build on offer, so there is nothing to qualify"
        );
    }

    fn stage_one_build(app: &mut App) -> u64 {
        let current_build = app.native_updater_service.snapshot().current_build;
        let build = current_build + 1;
        let CheckStart::Start(ticket) = app.native_updater_service.request_check() else {
            panic!("a fresh service must start exactly one check");
        };
        assert_eq!(
            app.native_updater_service.finish_check(
                ticket,
                crate::native_updater_service::DurableUpdateStatus {
                    enabled: true,
                    current_build,
                    staged_build: Some(build),
                    staged_version: Some(format!("1.0.{build}")),
                    staged_commit: Some("0123456789abcdef0123456789abcdef01234567".to_string()),
                    staged_dmg_sha256: Some("ab".repeat(32)),
                    changelog: None,
                    outcome: "staged".to_string(),
                    failing_checks: 0,
                    failing_persistent: false,
                    failing_kind: String::new(),
                    failing_applies: 0,
                    apply_failure: String::new(),
                    apply_failure_build: 0,
                    apply_failures_for_target: 0,
                    installable: true,
                    channel_unreadable: false,
                },
            ),
            crate::native_updater_service::CheckCompletion::Reduced,
            "PRECONDITION: the check must reduce, or nothing is staged and the \
             predicate under test is never reached"
        );
        build
    }

    /// THE PILL MUST NOT ASSERT THE PESSIMISTIC ANSWER WHEN A RETRY IS ALREADY
    /// SCHEDULED.
    ///
    /// "Update paused — manual retry" named a mechanism, not an action (there is
    /// no "retry" control in the UI), and it was flatly wrong half the time: a
    /// physical handoff failure inside its budget schedules another automatic
    /// attempt, and this arm claimed the lane had given up. Which answer is true
    /// is a question about SCHEDULING STATE, so every carrier of it is consulted —
    /// a live intent, or a latch with a deadline — and each must name THIS staged
    /// artifact.
    ///
    /// The user-facing strings are the assertion, because they are the contract.
    #[test]
    fn the_failure_pill_says_retrying_only_when_a_retry_is_actually_scheduled() {
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = App::headless_for_test();
        assert!(
            crate::app_config::update_auto_apply(&app.config),
            "PRECONDITION: the predicate requires the automatic lane enabled, and \
             the shipped default is ON"
        );
        let build = stage_one_build(&mut app);
        let failed = || UpdateOutcome::Failed {
            message: "overlap handoff failed safely: handoff proof ended TimedOut".to_string(),
        };
        let pill = |app: &App| {
            app.notice
                .as_ref()
                .map(crate::notice::TransientNotice::text)
                .expect("this case must paint a pill; silence is checked directly")
        };

        // NOTHING SCHEDULED: no intent, no latch. The honest answer is that the
        // user has to reach for the control.
        app.auto_apply_intent = None;
        app.auto_apply_manual_only = None;
        app.notice = None;
        app.surface_update_apply_outcome("automatic", failed(), false);
        assert_eq!(pill(&app), "↑ Update paused — see Version menu");

        // A LIVE INTENT for this artifact: a wake is already folded into the event
        // loop, so telling the user to act would be false.
        app.auto_apply_intent = Some(crate::AutoApplyIntent {
            build,
            dmg_sha256: [0xab; 32],
            retry_at: std::time::Instant::now() + std::time::Duration::from_secs(600),
            attempts: 0,
            apply_by: std::time::Instant::now() + std::time::Duration::from_secs(600),
        });
        app.notice = None;
        app.surface_update_apply_outcome("automatic", failed(), false);
        assert_eq!(pill(&app), "↑ Update delayed — retries on its own");

        // A LEFTOVER intent for a SUPERSEDED build schedules nothing for the
        // artifact on screen, so it must not borrow the optimistic answer.
        app.auto_apply_intent = Some(crate::AutoApplyIntent {
            build: build + 7,
            dmg_sha256: [0xab; 32],
            retry_at: std::time::Instant::now() + std::time::Duration::from_secs(600),
            attempts: 0,
            apply_by: std::time::Instant::now() + std::time::Duration::from_secs(600),
        });
        app.notice = None;
        app.surface_update_apply_outcome("automatic", failed(), false);
        assert_eq!(pill(&app), "↑ Update paused — see Version menu");

        // A LATCH WITH A DEADLINE is the other carrier: the lane is standing down
        // but `about_to_wait` will lapse it, so it does come back on its own.
        app.auto_apply_intent = None;
        app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
            build,
            dmg_sha256: [0xab; 32],
            retry_at: Some(std::time::Instant::now() + std::time::Duration::from_secs(600)),
        });
        app.notice = None;
        app.surface_update_apply_outcome("automatic", failed(), false);
        assert_eq!(pill(&app), "↑ Update delayed — retries on its own");

        // A DEADLINE-LESS latch (the policy-mismatch fail-safe) genuinely does not
        // come back by itself. This is the case the old wording described, and the
        // only one it was ever right about.
        app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
            build,
            dmg_sha256: [0xab; 32],
            retry_at: None,
        });
        app.notice = None;
        app.surface_update_apply_outcome("automatic", failed(), false);
        assert_eq!(pill(&app), "↑ Update paused — see Version menu");

        // …AND A SCHEDULED RETRY IS STILL NOT AUTOMATICALLY A PILL. The physical
        // lane can spend nine attempts before it gives up; repeating "retries on
        // its own" on each of them is a notification every ~40 minutes for most of
        // a day, about a state whose only advice is "wait". The first failure for
        // an artifact speaks; the rest are silent. (Convergence is not silent — it
        // clears `retry_at`, which is the deadline-less case two blocks up.)
        app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
            build,
            dmg_sha256: [0xab; 32],
            retry_at: Some(std::time::Instant::now() + std::time::Duration::from_secs(600)),
        });
        for (cycles, expected) in [
            (1_u8, Some("↑ Update delayed — retries on its own")),
            (2, None),
            (5, None),
            (8, None),
        ] {
            app.auto_apply_physical_retry = Some(crate::AutoOverlapRetry {
                build,
                dmg_sha256: [0xab; 32],
                cycles,
                last_attempt: std::time::Instant::now(),
            });
            app.notice = None;
            app.surface_update_apply_outcome("automatic handoff", failed(), false);
            assert_eq!(
                app.notice
                    .as_ref()
                    .map(crate::notice::TransientNotice::text)
                    .as_deref(),
                expected,
                "physical failure {cycles} for this artifact"
            );
        }
        // A record for a DIFFERENT artifact says nothing about this one, so the
        // suppression must not leak across builds.
        app.auto_apply_physical_retry = Some(crate::AutoOverlapRetry {
            build: build + 7,
            dmg_sha256: [0xab; 32],
            cycles: 8,
            last_attempt: std::time::Instant::now(),
        });
        app.notice = None;
        app.surface_update_apply_outcome("automatic handoff", failed(), false);
        assert_eq!(pill(&app), "↑ Update delayed — retries on its own");

        // AND A PERSON IS NEVER SILENCED BY THE AUTOMATIC LANE'S BUDGET.
        //
        // THIS BLOCK USED TO REST ON A FALSE PREMISE AND PASS ANYWAY. It asserted
        // that a person's lane "spends nothing", and then proved it by HAND-SETTING
        // a record 600 s old and calling the surfacing function directly — a fixture
        // that is stale by construction, so the pill was guaranteed whether or not a
        // person's failure charged the budget. It did charge it: the returned-handoff
        // completion dropped the attempt's `ApplyMode`, so a Version-menu apply that
        // failed physically spent `auto_apply_physical_retry` MICROSECONDS before
        // surfacing — landing inside the freshness window this predicate uses to
        // recognise the automatic lane's own quiet retries, and silencing the one
        // person who had just asked for the update.
        //
        // So the premise is now DRIVEN: a real person-initiated returned handoff,
        // through the same call the completion path's `(Some(attempt), None)` arm
        // makes, followed by the real surfacing. The inherited record is mid-budget
        // and 600 s old — the physical schedule's own MINIMUM spacing, so it is the
        // freshest record the automatic lane can leave behind between attempts — and
        // both assertions below fail if the person's failure re-stamps it.
        let manual_ticket = crate::native_updater_service::ApplyAttemptTicket::for_test(
            build,
            "0123456789abcdef0123456789abcdef01234567",
            &"ab".repeat(32),
        );
        manual_ticket.make_current_apply_for_test(&mut app.native_updater_service);
        app.auto_apply_physical_retry = Some(crate::AutoOverlapRetry {
            build,
            dmg_sha256: [0xab; 32],
            cycles: 5,
            last_attempt: std::time::Instant::now() - std::time::Duration::from_secs(600),
        });
        app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
            build,
            dmg_sha256: [0xab; 32],
            retry_at: Some(std::time::Instant::now() + std::time::Duration::from_secs(600)),
        });
        app.notice = None;
        let person = app.abort_reaped_native_apply_before_reconcile(
            &manual_ticket,
            "overlap handoff failed safely: handoff proof ended TimedOut".to_string(),
            crate::app_native::HandoffFailureLane::Manual,
        );
        assert_eq!(
            app.auto_apply_physical_retry.map(|retry| retry.cycles),
            Some(5),
            "a person's failure must not spend the automatic lane's budget — that \
             is how three clicks on a bad afternoon converged the background lane"
        );
        app.surface_update_apply_outcome("manual handoff", person, false);
        assert_eq!(
            pill(&app),
            "↑ Update delayed — retries on its own",
            "whoever just asked for the update is exactly who must be told it did \
             not happen; the automatic lane's mid-budget silence is not theirs"
        );
        app.auto_apply_physical_retry = None;

        // TURNING THE LANE OFF makes every scheduling carrier moot: a lapse would
        // only re-arm into a poll that answers `Clear`.
        app.config.update = Some(crate::app_config::UpdateConfig {
            auto_apply: Some(false),
            ..app.config.update.clone().unwrap_or_default()
        });
        assert!(
            !crate::app_config::update_auto_apply(&app.config),
            "PRECONDITION: the opt-out actually took"
        );
        app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
            build,
            dmg_sha256: [0xab; 32],
            retry_at: Some(std::time::Instant::now() + std::time::Duration::from_secs(600)),
        });
        app.notice = None;
        app.surface_update_apply_outcome("automatic", failed(), false);
        assert_eq!(pill(&app), "↑ Update paused — see Version menu");
    }

    /// SUBMISSION IS NOT COMPLETION, AND THE LEDGER MUST BE ABLE TO ESCALATE.
    ///
    /// The apply streak exists to reach `PERSISTENT_AFTER = 3` and say "aterm
    /// auto-update is failing". It could not: `Accepted` — the worker was merely
    /// STARTED, or an in-flight/`Applying` request was joined — recorded an apply
    /// SUCCESS, which zeroes the streak and clears the standing refusal. Every
    /// manual/control cycle was therefore wipe-then-set, capped at 1, and a single
    /// troubleshooting click erased a background streak that had already climbed.
    ///
    /// So the assertion is the whole cycle a person actually performs, three times
    /// over: click apply (`Accepted`), the asynchronous handoff fails a second later
    /// (`Failed`). Exactly three streak writes must come out of it and NOTHING may
    /// clear the lane — there is no verdict here that can, because the only place an
    /// apply is provable is a boot sentinel armed for the build that is now running
    /// (`aterm-update`'s `confirm_boot_health`), which this process cannot observe.
    #[test]
    fn a_submitted_then_failed_apply_writes_one_streak_increment_and_never_a_clear() {
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let failed = |reason: &str| UpdateOutcome::Failed {
            message: reason.to_string(),
        };
        let mut streak_writes = 0_u32;
        for attempt in 0..3 {
            let reason = format!("ChildDied (attempt {attempt})");
            for outcome in [UpdateOutcome::Accepted, failed(&reason)] {
                match apply_ledger_verdict(&outcome) {
                    ApplyLedgerVerdict::Failed(message) => {
                        assert_eq!(message, reason, "the typed outcome reaches the ledger");
                        streak_writes += 1;
                    }
                    // A refusal would be wrong here (it fills the non-streak slot) and
                    // a silent `Accepted` is the point of the fix.
                    other => assert_eq!(
                        other,
                        ApplyLedgerVerdict::Silent,
                        "only a real failure may touch the streak"
                    ),
                }
            }
        }
        assert_eq!(
            streak_writes, 3,
            "three failed applies must be three increments, or the escalation \
             threshold is unreachable on the lanes a person uses"
        );

        // The neighbours this must not disturb: a refusal still records its standing
        // explanation without touching the streak, and installed-but-needs-relaunch
        // says nothing durable (the bytes are in place; the staged marker reports it).
        assert_eq!(
            apply_ledger_verdict(&UpdateOutcome::Blocked {
                reasons: vec![
                    "Checkpoint Drafts: 1 document(s)".to_string(),
                    "busy".to_string()
                ],
            }),
            ApplyLedgerVerdict::Refused("Checkpoint Drafts: 1 document(s) · busy".to_string())
        );
        assert_eq!(
            apply_ledger_verdict(&UpdateOutcome::Deferred {
                reason: "the terminal is busy".to_string(),
            }),
            ApplyLedgerVerdict::Refused("the terminal is busy".to_string())
        );
        assert_eq!(
            apply_ledger_verdict(&UpdateOutcome::InstalledNeedsRelaunch {
                build: 7,
                message: "relaunch once".to_string(),
            }),
            ApplyLedgerVerdict::Silent
        );
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
