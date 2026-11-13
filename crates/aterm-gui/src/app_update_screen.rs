// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `App` glue for the update flow: one-click apply ([`App::apply_update_or_details`]),
//! live Version-menu re-sync ([`App::refresh_version_menu`]), and updater state
//! projection into the native Settings `/updates` route.

use crate::App;
use crate::native_app::UpdateOutcome;
use crate::update_screen::UpdateState;

/// Whether an apply outcome's `source` is automatic (`"automatic..."`) rather than
/// user-initiated (`"manual"`, `"manual handoff"`, `"control request"`).
fn source_is_automatic(source: &str) -> bool {
    source.starts_with("automatic")
}

/// Serializes tests that access the process-wide scratch update ledger (`ATERM_UPDATE_ROOT`),
/// absorbing poison so one panicking test does not cascade.
#[cfg(test)]
pub(crate) static UPDATE_LEDGER_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Hold [`UPDATE_LEDGER_TEST_LOCK`] after priming the scratch root
/// ([`crate::prime_test_update_root`]) and draining detached ledger writers
/// ([`crate::app_update_handoff::ledger_writers`]).
#[cfg(test)]
pub(crate) fn hold_update_ledger_for_test() -> std::sync::MutexGuard<'static, ()> {
    crate::prime_test_update_root();
    let held = UPDATE_LEDGER_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    #[cfg(unix)]
    crate::app_update_handoff::ledger_writers::wait_until_landed();
    held
}

#[cfg(test)]
impl App {
    /// The update lane's newest live row as `"<title> — <detail>"`, or `None`.
    pub(crate) fn update_row_text(&self) -> Option<String> {
        self.messages
            .live_rows()
            .filter(|l| l.msg.tag == aterm_messages::tags::UPDATE)
            .max_by_key(|l| l.id)
            .map(|l| format!("{} — {}", l.msg.title, l.msg.detail.join("; ")))
    }

    /// The update lane's newest recorded outcome as `"<title> — <detail>"`.
    pub(crate) fn update_record_text(&self) -> Option<String> {
        self.messages
            .log()
            .records()
            .rev()
            .find(|r| {
                r.tag == aterm_messages::tags::UPDATE
                    && r.retired() == Some(&aterm_messages::Retired::Recorded)
            })
            .map(|r| format!("{} — {}", r.title, r.detail.join("; ")))
    }

    /// The update lane's progress row detail joined with `; `, or `None`.
    pub(crate) fn update_row_detail(&self) -> Option<String> {
        self.messages
            .live_by_key(crate::update_words::KEY_PROGRESS)
            .map(|l| l.msg.detail.join("; "))
    }

    /// Reset the message center and update flow state between test cases.
    pub(crate) fn clear_messages_for_test(&mut self) {
        self.messages = crate::messages_host::new_center(
            aterm_messages::MessageLog::empty(),
            std::time::Instant::now(),
        );
        self.update_row_before_install = None;
        self.update_flow = None;
        self.staged_said = None;
        self.update_ctl_checks = 0;
        self.update_quiet = None;
        self.sync_messages();
    }
}

/// Whether the `ATERM_DEBUG_SEAMLESS_REEXEC` dev seam is armed (always `false` in
/// release builds) to re-exec the running binary through seamless handoff.
pub(crate) fn debug_seamless_reexec_armed() -> bool {
    static ARMED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ARMED.get_or_init(|| {
        let on = aterm_types::control_socket::env_flag_engaged(
            aterm_types::dev_seam!("ATERM_DEBUG_SEAMLESS_REEXEC")
                .map(|v| v.to_string_lossy().into_owned())
                .as_deref(),
        );
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

/// Update bar wording for a build already installed on disk awaiting manual activation.
#[cfg(any(unix, test))]
pub(crate) fn update_installed_title(version: Option<&str>) -> String {
    version.map_or_else(
        || "Update is ready".to_string(),
        crate::update_words::staged_title,
    )
}
#[cfg(any(unix, test))]
pub(crate) const UPDATE_INSTALLED_DETAIL: &str = crate::update_words::INSTALL_FROM_MENU;
/// Headline when automatic apply has stopped retrying and requires `Install now`.
pub(crate) const UPDATE_DIDNT_INSTALL: &str = "Couldn't install the update";
/// Headline when an apply attempt failed with no staged build left to retry.
pub(crate) const UPDATE_DIDNT_FINISH: &str = "Couldn't finish the update";
/// Headline when the install is blocked on unsaved user work.
pub(crate) const UPDATE_WAITS_FOR_YOU: &str = "Update waits for you";

/// Process-level inputs to [`App::apply_posture_for`], separated so
/// [`App::apply_posture_with`] is testable without mutating process environment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ApplyPostureEnv {
    /// Reason in-session handoff is unavailable ([`App::seamless_handoff_unavailable`]).
    pub(crate) handoff_unavailable: Option<crate::app_update_handoff::HandoffUnavailable>,
    /// Configured `[update] auto_apply` policy.
    pub(crate) auto_apply: crate::app_config::AutoApplySetting,
    /// `$ATERM_DEBUG_RELAUNCH_NUDGE` is set: the screenshot seam vetoes the lane.
    pub(crate) nudge_seam: bool,
}

impl ApplyPostureEnv {
    /// Observe live process and `App` posture inputs.
    pub(crate) fn observe(app: &App) -> Self {
        Self {
            handoff_unavailable: app.seamless_handoff_unavailable(),
            auto_apply: crate::app_config::update_auto_apply_setting(&app.config),
            nudge_seam: App::relaunch_nudge_seam_suppresses_auto_apply(),
        }
    }
}

impl App {
    /// Snapshot the updater reducer into a fresh [`UpdateState`] without disk I/O.
    pub(crate) fn update_snapshot(&self, checking: bool) -> UpdateState {
        let snapshot = self.native_updater_service.snapshot();
        let staged_build = snapshot.staged.as_ref().map(|staged| staged.build);
        let retry = self.apply_retry_for(staged_build);
        let on_hold = staged_build.is_some_and(|build| self.latched_activation_trial_spent(build));
        let kept = aterm_update::automatic_kept();
        UpdateState::from_service(snapshot, checking, retry)
            .with_staged_on_hold(on_hold)
            .with_automatic_checks(
                self.update_checks_running || kept == Some(true),
                crate::app_config::update_checks_automatic(&self.config),
            )
            .with_automatic_checks_waiting(kept.is_none())
            .with_download(self.update_quiet.clone())
            .with_dev_build(self.dev_build.clone())
            .with_install_posture(self.install_posture)
    }

    /// Whether a user-initiated update check (`start_native_update_check` or
    /// `aterm ctl update check`) is in flight.
    pub(crate) fn update_awaited(&self) -> bool {
        self.native_updater_service.snapshot().active.is_some() || self.update_ctl_checks > 0
    }

    /// A person's `aterm ctl update check` began or ended (`Wake::UpdateCheckAsked`).
    pub(crate) const fn note_update_check_asked(&mut self, begun: bool) {
        self.update_ctl_checks = if begun {
            self.update_ctl_checks.saturating_add(1)
        } else {
            self.update_ctl_checks.saturating_sub(1)
        };
    }

    /// Record unattended background download progress without posting a live row,
    /// republishing whole-MB progress to open Settings views and returning `true` when handled quietly.
    fn note_quiet_update_progress(&mut self, progress: &aterm_update::Progress) -> bool {
        use aterm_update::Progress as P;
        let quiet = match progress {
            P::Downloading {
                version,
                bytes_done,
                bytes_total,
            } => crate::update_screen::QuietDownload {
                version: version.clone(),
                title: crate::update_words::flow_title("Downloading", version),
                progress: if *bytes_total > 0 {
                    format!(
                        "{} / {}",
                        quiet_bytes((*bytes_done).min(*bytes_total)),
                        crate::toolchain_words::fmt_bytes(*bytes_total)
                    )
                } else {
                    quiet_bytes(*bytes_done)
                },
            },
            P::Verifying { version } => crate::update_screen::QuietDownload {
                version: version.clone(),
                title: crate::update_words::flow_title("Downloading", version),
                progress: String::new(),
            },
            P::Staged { .. } | P::Failed { .. } | P::Deferred { .. } | P::Withheld { .. } => {
                if self.update_quiet.take().is_some() {
                    self.publish_native_update_state();
                }
                return false;
            }
        };
        let flow_up = self.live_update_flow().is_some_and(|flow| {
            matches!(
                flow.phase,
                crate::messages_host::FlowPhase::Downloading
                    | crate::messages_host::FlowPhase::Checking
            )
        });
        if self.update_awaited() || flow_up {
            if self.update_quiet.take().is_some() {
                self.publish_native_update_state();
            }
            return false;
        }
        if self.update_quiet.as_ref() != Some(&quiet) {
            self.update_quiet = Some(quiet);
            self.publish_quiet_update_words();
        }
        true
    }

    /// Whether automatic apply will still attempt `staged_build` — via either a live
    /// `auto_apply_intent` or an `AutoApplyManualOnly` latch with a lapse deadline
    /// matching the current stage's build and digest while auto-apply is enabled.
    pub(crate) fn automatic_apply_retry_scheduled(&self, staged_build: u64) -> bool {
        let Some(digest) = self.automatic_apply_staged_digest(staged_build) else {
            return false;
        };
        self.auto_apply_intent
            .is_some_and(|intent| intent.build == staged_build && intent.dmg_sha256 == digest)
            || self.auto_apply_manual_only.is_some_and(|manual| {
                manual.build == staged_build
                    && manual.dmg_sha256 == digest
                    && manual.retry_at.is_some()
                    && !self.latched_activation_trial_spent(staged_build)
            })
    }

    fn automatic_apply_staged_digest(&self, staged_build: u64) -> Option<[u8; 32]> {
        if !crate::app_config::update_auto_apply(&self.config)
            || Self::relaunch_nudge_seam_suppresses_auto_apply()
        {
            return None;
        }
        self.native_updater_service
            .snapshot()
            .staged
            .as_ref()
            .filter(|stage| stage.build == staged_build)
            .and_then(|stage| crate::app_native::decode_dmg_sha256(&stage.dmg_sha256))
    }

    /// Typed retry disposition ([`crate::update_apply_trouble::ApplyRetry`]) for
    /// `staged_build` (`ManualOnly` when `None`, `NewerRelease` when the boot trial is spent).
    pub(crate) fn apply_retry_for(
        &self,
        staged_build: Option<u64>,
    ) -> crate::update_apply_trouble::ApplyRetry {
        use crate::update_apply_trouble::ApplyRetry;
        if let Some(remedy) =
            staged_build.and_then(|build| self.auto_apply_environment_remedy(build))
        {
            return ApplyRetry::NeedsPerson(remedy);
        }
        match staged_build {
            Some(build) if self.automatic_apply_retry_scheduled(build) => ApplyRetry::Scheduled,
            Some(build) if self.latched_activation_trial_spent(build) => ApplyRetry::NewerRelease,
            _ => ApplyRetry::ManualOnly,
        }
    }

    /// Standing block remedy for `staged_build` when [`Self::apply_retry_for`] returns `NeedsPerson`.
    fn person_remedy_for(
        &self,
        staged_build: u64,
    ) -> Option<crate::update_apply_trouble::PersonRemedy> {
        match self.apply_retry_for(Some(staged_build)) {
            crate::update_apply_trouble::ApplyRetry::NeedsPerson(remedy) => Some(remedy),
            _ => None,
        }
    }

    /// What this window tells the node job of its update lane (§4.13), with the staged build it
    /// speaks of: a person's setting holds updates here, or the posture of the staged build says
    /// whether the window's automatic lane may land it. Bytes a handoff of this window saw fail to
    /// start are no hold of the window's: the job's own lanes leave them for a window
    /// (`aterm_update`'s window-start failure record).
    #[cfg(unix)]
    pub(crate) fn window_update_lane(
        &self,
    ) -> (
        crate::fleet_listen::node_serve::WindowUpdateLane,
        Option<u64>,
    ) {
        let env = ApplyPostureEnv::observe(self);
        let holds =
            env.auto_apply == crate::app_config::AutoApplySetting::OffByConfig || env.nudge_seam;
        let stage = self.native_updater_service.snapshot().staged.as_ref();
        let staged = stage.map(|stage| stage.build);
        let posture = staged
            .filter(|_| !holds)
            .map(|build| self.apply_posture_with(build, env));
        let activation =
            posture.is_some() && stage.is_some_and(|stage| stage.is_installed_activation());
        (
            crate::fleet_listen::node_serve::WindowUpdateLane::of(holds, activation, posture),
            staged,
        )
    }

    /// Compute [`crate::update_words::ApplyPosture`] for the staged `build`.
    pub(crate) fn apply_posture_for(&self, build: u64) -> crate::update_words::ApplyPosture {
        self.apply_posture_with(build, ApplyPostureEnv::observe(self))
    }

    /// Pure posture evaluation: unavailable handoff outranks policy, screenshot seam,
    /// and per-build stand-down latch.
    pub(crate) fn apply_posture_with(
        &self,
        build: u64,
        env: ApplyPostureEnv,
    ) -> crate::update_words::ApplyPosture {
        use crate::app_config::AutoApplySetting;
        use crate::update_words::{ApplyPosture as P, AutoApplyVeto};
        if let Some(why) = env.handoff_unavailable {
            let veto = match env.auto_apply {
                AutoApplySetting::OffByConfig => Some(AutoApplyVeto::Config),
                AutoApplySetting::On if env.nudge_seam => Some(AutoApplyVeto::Env {
                    var: "ATERM_DEBUG_RELAUNCH_NUDGE",
                }),
                AutoApplySetting::On => None,
            };
            return P::HandoffDisabled {
                why,
                veto,
                image_kept: self.launch_keeps_image(),
            };
        }
        match env.auto_apply {
            AutoApplySetting::OffByConfig => return P::ManualByConfig,
            AutoApplySetting::On => {}
        }
        if env.nudge_seam {
            return P::VetoedByEnv {
                var: "ATERM_DEBUG_RELAUNCH_NUDGE",
            };
        }
        if let Some(manual) = self.auto_apply_manual_only.filter(|manual| {
            manual.build == build
                && self
                    .native_updater_service
                    .snapshot()
                    .staged
                    .as_ref()
                    .is_some_and(|stage| {
                        stage.build == build
                            && crate::app_native::decode_dmg_sha256(&stage.dmg_sha256)
                                == Some(manual.dmg_sha256)
                    })
        }) {
            if self.latched_activation_trial_spent(build) {
                return P::TrialSpent;
            }
            return P::ManualOnlyLatched {
                lapses: manual.retry_at.is_some(),
            };
        }
        if self.kept_image_installs_at_next_launch() {
            return P::AtNextLaunch;
        }
        P::Automatic
    }

    /// Consume a queued report only while its originating checker is current.
    pub(crate) fn note_update_progress_report(
        &mut self,
        report: aterm_update::ProgressReport,
        watch: &aterm_update::checker_watch::CheckerWatch,
    ) {
        if !report.is_current_for(watch) {
            return;
        }
        let progress = report.progress;
        if let aterm_update::Progress::Staged { version, build } = &progress {
            self.update_verified = Some((*build, std::time::Instant::now()));
            aterm_log::info!("update downloaded and verified: {version} (build {build})");
        }
        self.note_update_progress(&progress);
    }

    /// Advance the update flow row, heal superseded
    /// check/download health warnings, and record postponed or automatic-stage outcomes.
    pub(crate) fn note_update_progress(&mut self, progress: &aterm_update::Progress) {
        use crate::messages_host::FlowPhase;
        // A download announces itself before its first byte, and one the host refuses writes none,
        // so only bytes count: a scheduled check's failing asset must not renew a dead press's wait.
        let moved = match progress {
            aterm_update::Progress::Downloading { bytes_done, .. } => *bytes_done > 0,
            aterm_update::Progress::Verifying { .. } => true,
            _ => false,
        };
        if moved {
            self.native_updater_service
                .note_check_progress(std::time::Instant::now());
        }
        match progress {
            aterm_update::Progress::Failed { detail } => {
                aterm_log::warn!("update download failed: {detail}");
            }
            aterm_update::Progress::Deferred { detail } => {
                aterm_log::info!("update download postponed: {detail}");
            }
            aterm_update::Progress::Withheld { detail } => {
                aterm_log::info!("update download withheld: {detail}");
            }
            _ => {}
        }
        let posture = if let aterm_update::Progress::Staged { build, .. } = progress {
            Some(self.apply_posture_for(*build))
        } else {
            None
        };
        match progress {
            aterm_update::Progress::Downloading { .. }
            | aterm_update::Progress::Verifying { .. } => {
                self.heal_update_health(crate::messages_host::HealthProof::Checked);
            }
            aterm_update::Progress::Staged { .. } => {
                self.heal_update_health(crate::messages_host::HealthProof::Downloaded);
            }
            aterm_update::Progress::Deferred { .. }
            | aterm_update::Progress::Failed { .. }
            | aterm_update::Progress::Withheld { .. } => {}
        }
        let quiet_version = self.update_quiet.as_ref().map(|q| q.version.clone());
        if self.note_quiet_update_progress(progress) {
            return;
        }
        let downloading = self
            .live_update_flow()
            .filter(|flow| matches!(flow.phase, FlowPhase::Downloading | FlowPhase::Checking));
        let flow_version = downloading
            .map(|flow| flow.version.clone())
            .or(quiet_version)
            .unwrap_or_default();
        let downloading = downloading.map(|flow| flow.id);
        let msg = crate::update_words::progress(progress, posture, &flow_version);
        let staged_key = match (progress, posture) {
            (aterm_update::Progress::Staged { build, .. }, Some(posture)) => {
                Some((*build, posture))
            }
            _ => None,
        };
        if staged_key.is_some() {
            if staged_key == self.staged_said && self.staged_update_row().is_none() {
                if let Some(id) = downloading {
                    self.update_flow = None;
                    self.resolve_message(id, aterm_messages::Outcome::Ok);
                }
                return;
            }
            self.staged_said = staged_key;
        }
        if msg.hold == aterm_messages::Hold::LogOnly {
            // Log-only outcomes resolve or withdraw any live download row before recording.
            if let Some(id) = downloading {
                self.update_flow = None;
                match progress {
                    aterm_update::Progress::Staged { .. } => {
                        self.resolve_message(id, aterm_messages::Outcome::Ok);
                    }
                    aterm_update::Progress::Failed { .. } => {
                        self.resolve_message(id, aterm_messages::Outcome::Warn);
                    }
                    _ => {
                        self.withdraw_message(id);
                    }
                }
            }
            self.record_message(msg);
            return;
        }
        let (version, phase) = match progress {
            aterm_update::Progress::Downloading { version, .. } => {
                (version.clone(), FlowPhase::Downloading)
            }
            aterm_update::Progress::Verifying { version } => (version.clone(), FlowPhase::Checking),
            aterm_update::Progress::Staged { version, build } => {
                (version.clone(), FlowPhase::Staged { build: *build })
            }
            aterm_update::Progress::Failed { .. }
            | aterm_update::Progress::Deferred { .. }
            | aterm_update::Progress::Withheld { .. } => {
                return;
            }
        };
        let same_words = self
            .live_update_flow()
            .and_then(|flow| self.messages.live(flow.id))
            .is_some_and(|l| l.msg.title == msg.title && l.msg.detail == msg.detail);
        if same_words {
            self.restate_update_flow(crate::messages_host::restatement_of(&msg), None);
        } else {
            self.post_update_row(msg, &version, phase);
        }
    }

    /// Update the live ready row's wording when `build`'s [`ApplyPosture`](crate::update_words::ApplyPosture) changes,
    /// withdrawing a log-only row or raising the ready decision once if the lane stopped.
    pub(crate) fn restate_staged_bar_posture(&mut self, build: u64) {
        let posture = self.apply_posture_for(build);
        self.restate_staged_bar_posture_in(build, posture);
    }

    /// [`Self::restate_staged_bar_posture`] with an explicit `posture`.
    pub(crate) fn restate_staged_bar_posture_in(
        &mut self,
        build: u64,
        posture: crate::update_words::ApplyPosture,
    ) {
        use crate::messages_host::FlowPhase;
        let Some((id, version)) = self
            .live_update_flow()
            .filter(|flow| flow.phase == FlowPhase::Staged { build })
            .map(|flow| (flow.id, flow.version.clone()))
        else {
            if crate::update_words::staged_is_decision(Some(posture)) {
                self.ensure_staged_decision_in(build, posture);
            }
            return;
        };
        let words = crate::update_words::staged(&version, build, Some(posture));
        self.staged_said = Some((build, posture));
        if words.hold == aterm_messages::Hold::LogOnly {
            self.update_flow = None;
            self.withdraw_message(id);
            self.record_message(words);
            return;
        }
        let r = crate::update_words::restate_apply_posture(&version, build, posture);
        let same = self
            .live_update_flow()
            .and_then(|flow| self.messages.live(flow.id))
            .is_some_and(|l| {
                r.title.as_ref() == Some(&l.msg.title)
                    && r.detail.as_ref() == Some(&l.msg.detail)
                    && r.actions.as_ref() == Some(&l.msg.actions)
                    && r.severity == Some(l.msg.severity)
                    && r.meter.as_ref() == Some(&l.msg.meter)
            });
        if same {
            return;
        }
        self.restate_update_flow(r, Some(FlowPhase::Staged { build }));
    }

    /// Standing apply trouble for `staged_build` from the in-memory reducer snapshot,
    /// scoped to `apply_failure_build == staged_build` so a superseded stage's streak
    /// never decorates a newer offer.
    pub(crate) fn apply_trouble_for(
        &self,
        staged_build: u64,
    ) -> Option<crate::update_apply_trouble::ApplyTrouble> {
        use crate::update_apply_trouble::ApplyTrouble;
        let snapshot = self.native_updater_service.snapshot();
        if staged_build <= snapshot.current_build {
            return None;
        }
        if snapshot.apply_failure_build != staged_build {
            return None;
        }
        ApplyTrouble::new(
            snapshot.apply_failures_for_target,
            &snapshot.apply_failure,
            self.apply_retry_for(Some(staged_build)),
        )
        .map(|trouble| trouble.with_retry_in(self.automatic_apply_retry_in(staged_build)))
    }

    /// Remaining time until the stand-down latch retries `staged_build`.
    pub(crate) fn automatic_apply_retry_in(
        &self,
        staged_build: u64,
    ) -> Option<std::time::Duration> {
        let digest = self.automatic_apply_staged_digest(staged_build)?;
        self.auto_apply_manual_only
            .filter(|manual| manual.build == staged_build && manual.dmg_sha256 == digest)
            .and_then(|manual| manual.retry_at)
            .map(|at| at.saturating_duration_since(std::time::Instant::now()))
    }

    /// Reveal the Settings `/updates` route and start or join a manual update check.
    pub(crate) fn open_software_update_route_and_check(&mut self) -> Result<UpdateOutcome, String> {
        if !self.open_settings_tab(crate::native_settings::SettingsRoute::SoftwareUpdate) {
            return Err("could not open the native Software Update route".to_string());
        }
        Ok(self.start_native_update_check())
    }

    /// Whether the reducer holds a verified staged build strictly newer than running
    /// (or the `ATERM_DEBUG_SEAMLESS_REEXEC` seam is armed).
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

    /// One-click update handler: applies a strictly-newer staged build in place via
    /// seamless handoff, or opens Settings `/updates` when nothing is staged or the
    /// build's boot trial is spent.
    pub(crate) fn apply_update_or_details(&mut self) {
        // Windows: a build staged on this machine installs through its own installer.
        #[cfg(windows)]
        if self.windows_apply_staged_update() {
            return;
        }
        let debug_seamless = crate::app_update_screen::debug_seamless_reexec_armed();
        let staged_ready = self.staged_update_ready();
        let _ = self.apply_update_or_details_with_facts(staged_ready, debug_seamless);
    }

    /// [`Self::apply_update_or_details`] with pre-observed stage and debug-seam state.
    pub(crate) fn apply_update_or_details_with_facts(
        &mut self,
        staged_ready: bool,
        debug_seamless: bool,
    ) -> Option<crate::native_app::UpdateOutcome> {
        let on_hold = !debug_seamless
            && self
                .native_updater_service
                .snapshot()
                .staged
                .as_ref()
                .is_some_and(|staged| self.latched_activation_trial_spent(staged.build));
        if staged_ready && !on_hold {
            let outcome = if debug_seamless {
                self.apply_debug_seamless_update()
            } else {
                self.apply_native_update(crate::native_updater_service::ApplyMode::Immediate)
            };
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

    /// Persist an apply outcome to the durable ledger and update the UI.
    pub(crate) fn surface_update_apply_outcome(
        &mut self,
        source: &str,
        outcome: crate::native_app::UpdateOutcome,
        open_details: bool,
    ) {
        let target_build = self
            .native_updater_service
            .snapshot()
            .staged
            .as_ref()
            .map_or(0, |staged| staged.build);
        self.surface_update_apply_outcome_for_target(source, outcome, open_details, target_build);
    }

    /// [`Self::surface_update_apply_outcome`] attributed to an explicit `attempted_build`.
    pub(crate) fn surface_update_apply_outcome_for_target(
        &mut self,
        source: &str,
        outcome: crate::native_app::UpdateOutcome,
        open_details: bool,
        target_build: u64,
    ) {
        self.record_apply_outcome_for_target_in_ledger(&outcome, target_build);
        self.react_to_update_apply_outcome_for(source, outcome, open_details, Some(target_build));
    }

    /// Persist the apply outcome into `health.toml` (`record_apply_failure` for failed
    /// attempts, `record_apply_refusal` for non-streak deferrals/blocks) and feed any
    /// recorded failure back into the reducer immediately.
    pub(crate) fn record_apply_outcome_in_ledger(
        &mut self,
        outcome: &crate::native_app::UpdateOutcome,
    ) {
        let target_build = self
            .native_updater_service
            .snapshot()
            .staged
            .as_ref()
            .map_or(0, |staged| staged.build);
        self.record_apply_outcome_for_target_in_ledger(outcome, target_build);
    }

    fn record_apply_outcome_for_target_in_ledger(
        &mut self,
        outcome: &crate::native_app::UpdateOutcome,
        target_build: u64,
    ) {
        let current_build = self.native_updater_service.snapshot().current_build;
        let standing = self.apply_schedule_standing.take();
        match apply_ledger_verdict(outcome) {
            ApplyLedgerVerdict::Failed(message) => {
                // Feed the recorded failure back into the reducer immediately so menus and Settings update.
                if let Some(recorded) =
                    aterm_update::record_apply_failure(current_build, target_build, &message)
                    && self.native_updater_service.note_apply_failure(&recorded)
                {
                    self.publish_native_update_state();
                }
                if let Some((standing, detail)) = standing {
                    aterm_update::record_apply_refusal_with_detail(
                        current_build,
                        &standing,
                        &detail,
                    );
                }
            }
            ApplyLedgerVerdict::Refused(reason) => {
                aterm_update::record_apply_refusal(current_build, &reason);
            }
            ApplyLedgerVerdict::Silent => {
                if let Some((standing, detail)) = standing {
                    aterm_update::record_apply_refusal_with_detail(
                        current_build,
                        &standing,
                        &detail,
                    );
                }
            }
        }
    }

    /// Update UI and logs for an apply outcome without writing the durable health ledger
    /// (used directly by `ATERM_DEBUG_SEAMLESS_REEXEC` so simulated handoffs do not
    /// accumulate persistent apply failure streaks).
    pub(crate) fn react_to_update_apply_outcome(
        &mut self,
        source: &str,
        outcome: crate::native_app::UpdateOutcome,
        open_details: bool,
    ) {
        self.react_to_update_apply_outcome_for(source, outcome, open_details, None);
    }

    /// [`Self::react_to_update_apply_outcome`] for `attempted` (handling a stage that
    /// was superseded while the worker ran).
    pub(crate) fn react_to_update_apply_outcome_for(
        &mut self,
        source: &str,
        outcome: crate::native_app::UpdateOutcome,
        open_details: bool,
        attempted: Option<u64>,
    ) {
        let staged = self
            .native_updater_service
            .snapshot()
            .staged
            .as_ref()
            .map(|stage| (stage.build, stage.version.clone()));
        let retry_scheduled = staged
            .as_ref()
            .is_some_and(|(build, _)| self.automatic_apply_retry_scheduled(*build));
        match outcome {
            crate::native_app::UpdateOutcome::Accepted => {
                aterm_log::info!("update apply ({source}): accepted");
            }
            #[cfg(any(unix, test))]
            crate::native_app::UpdateOutcome::InstalledNeedsRelaunch { build, message } => {
                let retry_scheduled = self.automatic_apply_retry_scheduled(build);
                aterm_log::warn!(
                    "update apply ({source}): {message} (build {build}; retry scheduled: \
                     {retry_scheduled})"
                );
                if open_details {
                    let _ = self
                        .open_settings_tab(crate::native_settings::SettingsRoute::SoftwareUpdate);
                } else if retry_scheduled {
                    self.restate_staged_bar_posture(build);
                } else {
                    self.retire_staged_update_row(true);
                    let version = staged
                        .as_ref()
                        .filter(|(staged_build, _)| *staged_build == build)
                        .map(|(_, version)| version.as_str());
                    self.note_update_outcome(crate::update_words::needs_install(
                        &update_installed_title(version),
                        UPDATE_INSTALLED_DETAIL,
                        aterm_messages::Severity::Info,
                        build,
                    ));
                }
            }
            crate::native_app::UpdateOutcome::Deferred { reason, .. } => {
                aterm_log::info!("update apply ({source}) deferred: {reason}");
                self.retire_update_installing();
                if open_details {
                    let _ = self
                        .open_settings_tab(crate::native_settings::SettingsRoute::SoftwareUpdate);
                } else if !retry_scheduled && self.staged_update_row().is_none() {
                    let msg = self.stopped_lane_outcome(UPDATE_DIDNT_INSTALL);
                    self.note_update_outcome(msg);
                }
            }
            crate::native_app::UpdateOutcome::Blocked { reasons } => {
                let message = reasons.join(" · ");
                aterm_log::warn!("update apply ({source}) waiting: {message}");
                if open_details {
                    self.retire_charging_surge();
                    let _ = self
                        .open_settings_tab(crate::native_settings::SettingsRoute::SoftwareUpdate);
                } else {
                    self.note_update_blockers(&reasons, retry_scheduled);
                }
            }
            #[cfg(any(unix, test))]
            crate::native_app::UpdateOutcome::CaptureRefused { message } => {
                aterm_log::warn!("update apply ({source}) could not carry a session: {message}");
                #[cfg(unix)]
                if open_details || !retry_scheduled {
                    self.record_update_switch_stopped(retry_scheduled, &message);
                }
                if open_details {
                    self.retire_charging_surge();
                    let _ = self
                        .open_settings_tab(crate::native_settings::SettingsRoute::SoftwareUpdate);
                } else if retry_scheduled {
                    self.retire_charging_surge();
                    if let Some((staged_build, _)) = &staged {
                        self.restate_staged_bar_posture(*staged_build);
                    }
                    let spacing_left = self.capture_refusal_spacing_left(std::time::Instant::now());
                    #[cfg(unix)]
                    let switch = self.update_switch_on_record.take();
                    #[cfg(not(unix))]
                    let switch: Option<Option<String>> = None;
                    let switch = switch
                        .or_else(|| staged.as_ref().map(|(_, version)| Some(version.clone())));
                    self.note_update_outcome(crate::update_words::outcome(
                        &crate::update_words::waiting_to_install(
                            switch.as_ref().map(Option::as_deref),
                        ),
                        &crate::update_words::capture_refused_waiting(&message, spacing_left),
                        aterm_messages::Severity::Info,
                    ));
                } else {
                    let msg = self.stopped_lane_outcome(UPDATE_DIDNT_INSTALL);
                    self.note_update_outcome(msg);
                }
            }
            crate::native_app::UpdateOutcome::Failed { message } => {
                aterm_log::warn!("update apply ({source}) failed safely: {message}");
                let superseded = attempted.filter(|&tried| {
                    tried != 0
                        && staged
                            .as_ref()
                            .is_some_and(|(staged_build, _)| *staged_build != tried)
                });
                if open_details {
                    self.retire_charging_surge();
                    let _ = self
                        .open_settings_tab(crate::native_settings::SettingsRoute::SoftwareUpdate);
                } else if let Some((tried, replacement)) = superseded.zip(staged.as_ref()) {
                    let replacement = replacement.0;
                    let posture = self.apply_posture_for(replacement);
                    self.say_superseded_apply_failure(
                        source,
                        &message,
                        tried,
                        replacement,
                        posture,
                    );
                } else if let Some((staged_build, version)) = staged {
                    // If a retry is scheduled, restate the flow row (and notify on manual press);
                    // if the lane stopped, raise the `Install now` failure row.
                    if retry_scheduled {
                        self.retire_charging_surge();
                        self.restate_staged_bar_posture(staged_build);
                        if !source_is_automatic(source) {
                            self.note_update_outcome(crate::update_words::outcome(
                                &format!("Couldn't install aterm v{version}"),
                                crate::update_words::TRIES_AGAIN,
                                aterm_messages::Severity::Info,
                            ));
                        }
                    } else {
                        let detail = self
                            .person_remedy_for(staged_build)
                            .or_else(|| {
                                crate::update_apply_trouble::ApplyTrouble::needs_person(&message)
                                    .then(|| {
                                        crate::update_apply_trouble::PersonRemedy::of_refusal(
                                            &message,
                                        )
                                    })
                            })
                            .map_or(
                                crate::update_words::INSTALL_FROM_MENU,
                                crate::update_apply_trouble::PersonRemedy::sentence,
                            );
                        self.note_update_outcome(crate::update_words::needs_install_because(
                            &format!("Couldn't install aterm v{version}"),
                            crate::update_words::short_cause(&message),
                            detail,
                            aterm_messages::Severity::Warn,
                            staged_build,
                        ));
                    }
                } else {
                    // With no staged build left, unattended failures are log-only; manual presses show `UPDATE_DIDNT_FINISH`.
                    if source_is_automatic(source) {
                        self.note_update_outcome(crate::update_words::outcome(
                            UPDATE_DIDNT_FINISH,
                            &crate::update_words::with_named_cause(
                                "the next check downloads it again",
                                &message,
                            ),
                            aterm_messages::Severity::Warn,
                        ));
                    } else {
                        self.note_update_outcome(crate::update_words::failed(
                            UPDATE_DIDNT_FINISH,
                            "",
                            false,
                        ));
                    }
                    self.retire_staged_update_row(false);
                }
            }
        }
    }

    /// Surface a close-preflight refusal: show user-actionable blockers (unsaved editor
    /// or Settings drafts), or mark a trial-spent build on hold.
    pub(crate) fn note_update_blockers(&mut self, reasons: &[String], retry_scheduled: bool) {
        if let Some(held) = self
            .native_updater_service
            .snapshot()
            .staged
            .as_ref()
            .map(|stage| stage.build)
            .filter(|&build| self.latched_activation_trial_spent(build))
        {
            self.restate_staged_bar_posture_in(held, crate::update_words::ApplyPosture::TrialSpent);
        } else if let Some(blocker) = Self::update_blocker_for_person(reasons) {
            if retry_scheduled {
                self.say_update_waits_for_you(blocker);
            } else {
                self.note_update_outcome(crate::update_words::failed(
                    UPDATE_WAITS_FOR_YOU,
                    blocker,
                    true,
                ));
            }
        } else if !retry_scheduled {
            let msg = self.stopped_lane_outcome(UPDATE_DIDNT_INSTALL);
            self.note_update_outcome(msg);
        }
    }

    /// Raise the standing "Update waits for you" row for `blocker`, clearing any
    /// quiet period from an earlier wait.
    pub(crate) fn say_update_waits_for_you(&mut self, blocker: &str) {
        if !self.update_wait_said {
            let _ = self.messages.end_standing_quiet(
                crate::update_words::KEY_WAITS_FOR_YOU,
                crate::messages_host::wall_stamp_now(),
                std::time::Instant::now(),
            );
            self.update_wait_said = true;
        }
        self.note_update_outcome(crate::update_words::waits_for_you(blocker));
    }

    /// Withdraw the standing "Update waits for you" row once user blockers clear.
    pub(crate) fn withdraw_update_waits_for_you(&mut self) {
        self.update_wait_said = false;
        if let Some(id) = self
            .messages
            .live_by_key(crate::update_words::KEY_WAITS_FOR_YOU)
            .map(|l| l.id)
        {
            self.withdraw_message(id);
        }
    }

    /// Withdraw "Update waits for you" if automatic retry is no longer scheduled
    /// for the waited build.
    pub(crate) fn settle_update_wait_on_person(&mut self) {
        if self.update_waits_on_person.is_none() && !self.update_wait_said {
            return;
        }
        let waited = self.update_waits_on_person.or_else(|| {
            self.native_updater_service
                .snapshot()
                .staged
                .as_ref()
                .map(|stage| stage.build)
        });
        if waited.is_some_and(|build| self.automatic_apply_retry_scheduled(build)) {
            return;
        }
        aterm_log::info!(
            "update: the automatic lane no longer retries build {waited:?} by itself; the \
             notice that it waits for unsaved work leaves"
        );
        self.update_waits_on_person = None;
        self.withdraw_update_waits_for_you();
    }

    /// Build the `Install now` decision row (or log record when nothing is staged) for a stopped lane.
    pub(crate) fn stopped_lane_outcome(&self, title: &str) -> aterm_messages::Message {
        let staged = self
            .native_updater_service
            .snapshot()
            .staged
            .as_ref()
            .map(|stage| stage.build);
        match staged {
            Some(build) => crate::update_words::needs_install(
                title,
                self.person_remedy_for(build).map_or(
                    crate::update_words::INSTALL_FROM_MENU,
                    crate::update_apply_trouble::PersonRemedy::sentence,
                ),
                aterm_messages::Severity::Warn,
                build,
            ),
            None => crate::update_words::outcome(
                title,
                crate::update_words::INSTALL_FROM_MENU,
                aterm_messages::Severity::Warn,
            ),
        }
    }

    /// Handle a failed attempt whose target build `tried` was superseded by `replacement`:
    /// withdraw `tried`'s row, record/show the failure without `replacement`'s version,
    /// and restate `replacement`'s posture.
    pub(crate) fn say_superseded_apply_failure(
        &mut self,
        source: &str,
        message: &str,
        tried: u64,
        replacement: u64,
        posture: crate::update_words::ApplyPosture,
    ) {
        use crate::messages_host::FlowPhase;
        self.retire_update_installing();
        if let Some(id) = self
            .live_update_flow()
            .filter(|flow| flow.phase == FlowPhase::Staged { build: tried })
            .map(|flow| flow.id)
        {
            self.update_flow = None;
            self.withdraw_message(id);
        }
        aterm_log::info!(
            "update apply ({source}): build {tried} failed after build {replacement} replaced it"
        );
        if source_is_automatic(source) {
            self.note_update_outcome(crate::update_words::outcome(
                UPDATE_DIDNT_FINISH,
                &crate::update_words::with_named_cause("another download replaced it", message),
                aterm_messages::Severity::Warn,
            ));
        } else {
            self.note_update_outcome(crate::update_words::failed(UPDATE_DIDNT_FINISH, "", false));
        }
        self.restate_staged_bar_posture_in(replacement, posture);
    }

    /// Post or log an apply-lane outcome (`update.outcome`) and retire the charging surge.
    pub(crate) fn note_update_outcome(&mut self, msg: aterm_messages::Message) {
        self.retire_update_installing();
        if msg.hold == aterm_messages::Hold::LogOnly {
            self.record_message(msg);
            return;
        }
        if msg
            .actions
            .iter()
            .any(|a| matches!(a, aterm_messages::Intent::ApplyUpdate { .. }))
            && let Some(id) = self.staged_update_row()
        {
            self.update_flow = None;
            self.withdraw_message(id);
        }
        let normalized = msg.clone().normalized();
        let repeats = self
            .messages
            .live_by_key(crate::update_words::KEY_OUTCOME)
            .is_some_and(|l| {
                l.msg.title == normalized.title
                    && l.msg.detail == normalized.detail
                    && l.msg.actions == normalized.actions
            });
        if !repeats {
            self.resolve_held_row_under(crate::update_words::KEY_OUTCOME);
        }
        self.post_message(msg);
    }

    /// Whether pressing the update row applies the staged build in place (requires
    /// `FlowPhase::Staged`, `staged_ready`, and `handoff_available`).
    pub(crate) const fn update_bar_press_applies(
        bar_is_staged: bool,
        staged_ready: bool,
        handoff_available: bool,
    ) -> bool {
        bar_is_staged && staged_ready && handoff_available
    }

    /// Upgrade rim motion amplitude (`0.0` under serious mode or Reduce Motion so the
    /// rim stays visible without animation).
    fn upgrade_rim_motion(&self) -> f32 {
        if self
            .serious_mode_policy()
            .allows(crate::motion::SeriousEffect::LevelUp)
        {
            self.motion_policy(true)
                .amplitude(crate::motion::MotionEffect::UpgradeSurge)
        } else {
            0.0
        }
    }

    /// Start charging the upgrade rim for `build`.
    #[cfg(any(unix, test))]
    pub(crate) fn spawn_upgrade_surge(&mut self, build: u64) {
        self.level_up = Some(crate::level_up::LevelUp::charging(
            build,
            std::time::Instant::now(),
            self.upgrade_rim_motion(),
        ));
        self.request_redraw_all_windows();
    }

    /// Start the successor's upgrade rim already charged so the handoff swap is continuous.
    pub(crate) fn spawn_upgrade_surge_continued(&mut self, build: u64) {
        self.level_up = Some(crate::level_up::LevelUp::charging_continued(
            build,
            std::time::Instant::now(),
            self.upgrade_rim_motion(),
        ));
        self.request_redraw_all_windows();
    }

    /// Retire the upgrade rim charging surge.
    pub(crate) fn retire_charging_surge(&mut self) {
        if self.level_up.take().is_some() {
            self.request_redraw_all_windows();
        }
    }

    /// Mark the update row as installing before building the handoff carry (adding a row
    /// only for explicit user-initiated applies; the upgrade rim starts at [`Self::charge_upgrade_rim_at_park`]).
    #[cfg(unix)]
    pub(crate) fn begin_update_installing(&mut self, explicit: bool) {
        use crate::messages_host::FlowPhase;
        let version = self
            .native_updater_service
            .snapshot()
            .staged
            .as_ref()
            .map(|staged| staged.version.clone())
            .unwrap_or_default();
        let installing = crate::update_words::installing(&version);
        self.update_wait_said = false;
        if let Some(id) = self
            .messages
            .live_by_key(crate::update_words::KEY_WAITS_FOR_YOU)
            .map(|l| l.id)
        {
            self.messages.withdraw(id, std::time::Instant::now());
        }
        if self
            .live_update_flow()
            .is_some_and(|flow| flow.phase == FlowPhase::Installing)
        {
            return;
        }
        self.update_row_before_install = None;
        let live = self
            .live_update_flow()
            .cloned()
            .and_then(|flow| self.messages.live(flow.id).map(|l| (flow, l.msg.clone())));
        if let Some((flow, previous)) = live {
            let id = flow.id;
            self.update_row_before_install = Some((previous, flow));
            self.messages.restate(
                id,
                crate::messages_host::restatement_of(&installing),
                std::time::Instant::now(),
            );
            if let Some(flow) = self.update_flow.as_mut() {
                flow.phase = FlowPhase::Installing;
            }
        } else if explicit {
            self.post_update_row(installing, &version, FlowPhase::Installing);
        }
        self.request_redraw_all_windows();
    }

    /// Start or re-anchor the upgrade rim for `build` when PTY readers park.
    #[cfg(unix)]
    pub(crate) fn charge_upgrade_rim_at_park(&mut self, build: u64) {
        match self.level_up.as_mut() {
            Some(rim) => rim.re_anchor(std::time::Instant::now()),
            None => self.spawn_upgrade_surge(build),
        }
    }

    /// Restore the update row from `FlowPhase::Installing` and retire the charging surge
    /// when a handoff is refused or aborted.
    pub(crate) fn retire_update_installing(&mut self) {
        use crate::messages_host::FlowPhase;
        let previous = self.update_row_before_install.take();
        let now = std::time::Instant::now();
        if let Some(id) = self
            .live_update_flow()
            .filter(|flow| flow.phase == FlowPhase::Installing)
            .map(|flow| flow.id)
        {
            match previous {
                Some((words, flow)) => {
                    self.messages
                        .restate(id, crate::messages_host::restatement_of(&words), now);
                    self.update_flow = Some(flow);
                }
                None => {
                    self.messages
                        .resolve(id, aterm_messages::Outcome::Warn, now);
                    self.update_flow = None;
                }
            }
        }
        self.retire_charging_surge();
        self.request_redraw_all_windows();
    }

    /// Title the menu bar's machine item from [`Self::machine_view`] (§5.1 of
    /// `docs/DESIGN-machine-names-2026-11-04.md`); nothing until its reader's first read.
    #[cfg(target_os = "macos")]
    pub(crate) fn refresh_machine_menu(&self) {
        if let (Some(handle), Some(view)) = (self._menu.as_ref(), self.machine_view.as_ref()) {
            crate::menu::update_machine_menu(handle, view);
        }
    }

    /// Sync the macOS Version menu title and items with the current staged/realized update state.
    pub(crate) fn refresh_version_menu(&self) {
        if let Some(handle) = self._menu.as_ref() {
            let running = self.native_updater_service.snapshot().current_build;
            let staged = crate::menu::staged_for_version_menu(
                self.relaunch
                    .as_ref()
                    .map(|r| (r.build, r.version.as_str())),
                running,
            )
            .map(|(build, version)| (build, version.to_string()));
            let realized = !self.serious_mode_enabled()
                && self
                    .upgrade_realized
                    .is_some_and(|t| t.elapsed() < crate::relaunch_notice::REALIZED_ARROW_TTL);
            let trouble = staged
                .as_ref()
                .and_then(|(build, _)| self.apply_trouble_for(*build));
            let on_hold = staged
                .as_ref()
                .is_some_and(|(build, _)| self.latched_activation_trial_spent(*build));
            crate::menu::update_version_menu(
                handle,
                staged.as_ref().map(|(b, v)| (*b, v.as_str())),
                trouble.as_ref(),
                on_hold,
                realized,
            );
        }
    }

    /// Retitle the outgoing process's Version menu to the proven successor's `v<target>`.
    #[cfg(unix)]
    pub(crate) fn publish_handoff_target_to_menu_bar(&self) -> Option<String> {
        let pending = self.pending_update_handoff.as_ref()?;
        let title = crate::menu::handoff_target_bar_title(
            pending.target_build,
            pending.same_image.is_some(),
            self.relaunch
                .as_ref()
                .map(|r| (r.build, r.version.as_str())),
        )?;
        if let Some(handle) = self._menu.as_ref() {
            crate::menu::show_handoff_target_version(handle, &title);
        }
        Some(title)
    }
}

/// Format quiet download progress in whole MB (or 0.1 GB from 1 GB) so Settings
/// republishes at most once per MB.
fn quiet_bytes(n: u64) -> String {
    const MB: u64 = 1_000_000;
    match n - n % MB {
        0 => "0 MB".to_string(),
        whole => crate::toolchain_words::fmt_bytes(whole),
    }
}

/// Durable health-ledger action for an [`UpdateOutcome`].
#[derive(Debug, PartialEq, Eq)]
enum ApplyLedgerVerdict {
    /// Attempt failed; increments the apply failure streak.
    Failed(String),
    /// Attempt refused before execution; records the standing refusal without incrementing the streak.
    Refused(String),
    /// Nothing durable to say yet.
    Silent,
}

/// Map an [`UpdateOutcome`] to its [`ApplyLedgerVerdict`] (`Accepted` and
/// `InstalledNeedsRelaunch` are `Silent` until `confirm_boot_health` confirms boot).
fn apply_ledger_verdict(outcome: &UpdateOutcome) -> ApplyLedgerVerdict {
    match outcome {
        UpdateOutcome::Failed { message } => ApplyLedgerVerdict::Failed(message.clone()),
        #[cfg(any(unix, test))]
        UpdateOutcome::CaptureRefused { message } => ApplyLedgerVerdict::Failed(message.clone()),
        UpdateOutcome::Blocked { reasons } => ApplyLedgerVerdict::Refused(reasons.join(" · ")),
        UpdateOutcome::Deferred { reason, .. } => ApplyLedgerVerdict::Refused(reason.clone()),
        UpdateOutcome::Accepted => ApplyLedgerVerdict::Silent,
        #[cfg(any(unix, test))]
        UpdateOutcome::InstalledNeedsRelaunch { .. } => ApplyLedgerVerdict::Silent,
    }
}

#[cfg(test)]
#[path = "app_update_screen_tests.rs"]
pub(crate) mod tests;
