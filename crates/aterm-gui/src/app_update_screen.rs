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

/// Serializes tests that access the process-wide scratch update ledger
/// (`ATERM_UPDATE_ROOT`), whose standing slots (`last_apply_failure_target_build`)
/// are single-valued across parallel tests. Absorbs poison so one panicking test
/// does not cascade.
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
        self.messages = aterm_messages::MessageCenter::new(
            aterm_messages::MessageLog::empty(),
            std::time::Instant::now(),
        );
        self.update_row_before_install = None;
        self.update_flow = None;
        self.staged_said = None;
        self.update_ctl_checks = 0;
        self.update_quiet = None;
        if self.message_band_rows != 0 {
            self.message_band_rows = 0;
            self.regrid_for_chrome_rows();
        }
        self.sync_messages();
    }
}

/// `true` while the `ATERM_DEBUG_SEAMLESS_REEXEC` dev seam is armed (always `false`
/// in release builds). Re-execs the running binary through the seamless handoff
/// and adopt path.
pub(crate) fn debug_seamless_reexec_armed() -> bool {
    static ARMED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ARMED.get_or_init(|| {
        // Read the environment directly (`env_flag_engaged` rejects unset, empty, and "0")
        // rather than calling `debug_seamless_reexec_armed` recursively inside `OnceLock`.
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
    /// Why in-session handoff is unavailable ([`App::seamless_handoff_unavailable`]).
    /// When `Some`, cold apply is refused while any PTY is open.
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
        UpdateState::from_service(snapshot, checking, retry)
            .with_staged_on_hold(on_hold)
            .with_automatic_checks(
                self.update_checks_running,
                crate::app_config::update_checks_automatic(&self.config),
            )
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
    pub(crate) fn note_update_check_asked(&mut self, begun: bool) {
        self.update_ctl_checks = if begun {
            self.update_ctl_checks.saturating_add(1)
        } else {
            self.update_ctl_checks.saturating_sub(1)
        };
    }

    /// Record progress for an unattended background download without raising a band row.
    /// Republishes whole-MB progress to open Settings views only when the text changes.
    /// Returns `true` when handled quietly.
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
        if !crate::app_config::update_auto_apply(&self.config)
            || Self::relaunch_nudge_seam_suppresses_auto_apply()
        {
            return false;
        }
        let Some(digest) = self
            .native_updater_service
            .snapshot()
            .staged
            .as_ref()
            .filter(|stage| stage.build == staged_build)
            .and_then(|stage| crate::app_native::decode_dmg_sha256(&stage.dmg_sha256))
        else {
            return false;
        };
        self.auto_apply_intent
            .is_some_and(|intent| intent.build == staged_build && intent.dmg_sha256 == digest)
            || self.auto_apply_manual_only.is_some_and(|manual| {
                manual.build == staged_build
                    && manual.dmg_sha256 == digest
                    && manual.retry_at.is_some()
            })
    }

    /// Typed retry disposition ([`crate::update_apply_trouble::ApplyRetry`]) for
    /// `staged_build` (`ManualOnly` when `None`, `NewerRelease` when the boot trial is spent).
    pub(crate) fn apply_retry_for(
        &self,
        staged_build: Option<u64>,
    ) -> crate::update_apply_trouble::ApplyRetry {
        use crate::update_apply_trouble::ApplyRetry;
        // A standing environmental block's remedy takes precedence over the ledger's last failure reason.
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
            // Carry the policy veto into `HandoffOff` so it does not promise a cold apply when vetoed.
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
        if self.kept_image_waits_for_terminal() {
            return P::WaitsForTerminal;
        }
        P::Automatic
    }

    /// Handle `Wake::UpdateProgress`: advance the update flow row, heal superseded
    /// check/download health warnings, and record postponed or automatic-stage outcomes.
    pub(crate) fn note_update_progress(&mut self, progress: &aterm_update::Progress) {
        use crate::messages_host::FlowPhase;
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
        // Announce a staged build at most once per `(build, posture)`.
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
        // Byte-count ticks under identical wording restate the live row in place without
        // logging; phase changes supersede the row.
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

    /// Update the live ready row's wording when `build`'s [`ApplyPosture`] changes.
    /// Withdraws the row if the posture becomes log-only, or raises the ready decision
    /// once if the lane just stopped with no row up.
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
        if !self.automatic_apply_retry_scheduled(staged_build) {
            return None;
        }
        self.auto_apply_manual_only
            .filter(|manual| manual.build == staged_build)
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
            // Debug re-exec routes through `react_to_update_apply_outcome` to avoid writing the durable ledger.
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
        // Record any queued schedule standing note after `record_apply_failure` (which clears
        // prior refusals); a refusal outcome supersedes the note.
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
                if let Some(standing) = standing {
                    aterm_update::record_apply_refusal(current_build, &standing);
                }
            }
            ApplyLedgerVerdict::Refused(reason) => {
                aterm_update::record_apply_refusal(current_build, &reason);
            }
            ApplyLedgerVerdict::Silent => {
                if let Some(standing) = standing {
                    aterm_update::record_apply_refusal(current_build, &standing);
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
        // Scheduled retries stay log-only; only actionable blockers or a stopped lane raise a band row.
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
            crate::native_app::UpdateOutcome::Deferred { reason } => {
                aterm_log::info!("update apply ({source}) deferred: {reason}");
                // Restore any row rewritten to "installing", and raise a decision row only if the lane stopped.
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
                    // When a retry is scheduled, restate the flow row and log the capture refusal.
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
                        // Standing block remedy takes precedence over the attempt's raw failure message.
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
                    // Withdraw any ready row for the consumed artifact.
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
    fn stopped_lane_outcome(&self, title: &str) -> aterm_messages::Message {
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
        // Withdraw any live ready row before posting an outcome that carries `Install now`.
        if msg
            .actions
            .iter()
            .any(|a| matches!(a, aterm_messages::Intent::ApplyUpdate { .. }))
            && let Some(id) = self.staged_update_row()
        {
            self.update_flow = None;
            self.withdraw_message(id);
        }
        // Resolve a distinct prior held outcome before superseding it; identical words
        // re-anchor the existing row in place.
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
    pub(crate) fn update_bar_press_applies(
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
    /// only for explicit user-initiated applies). The upgrade rim starts later at reader
    /// park ([`Self::charge_upgrade_rim_at_park`]).
    #[cfg(any(unix, test))]
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
        // Preflight passed: withdraw any standing user-blocker wait row before building carry.
        self.update_wait_said = false;
        if let Some(id) = self
            .messages
            .live_by_key(crate::update_words::KEY_WAITS_FOR_YOU)
            .map(|l| l.id)
        {
            self.messages.withdraw(id, std::time::Instant::now());
        }
        // On a held press re-entry, keep the pre-install row snapshot captured on the first entry.
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
    #[cfg(any(unix, test))]
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
        // Defer row geometry sync to the trailing outcome/handoff wake to avoid a double re-grid.
        self.request_redraw_all_windows();
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

/// Map an [`UpdateOutcome`] to its [`ApplyLedgerVerdict`]. `Accepted` and
/// `InstalledNeedsRelaunch` are `Silent`: apply success is confirmed only by
/// `confirm_boot_health` on the new build.
fn apply_ledger_verdict(outcome: &UpdateOutcome) -> ApplyLedgerVerdict {
    match outcome {
        UpdateOutcome::Failed { message } => ApplyLedgerVerdict::Failed(message.clone()),
        #[cfg(any(unix, test))]
        UpdateOutcome::CaptureRefused { message } => ApplyLedgerVerdict::Failed(message.clone()),
        UpdateOutcome::Blocked { reasons } => ApplyLedgerVerdict::Refused(reasons.join(" · ")),
        UpdateOutcome::Deferred { reason } => ApplyLedgerVerdict::Refused(reason.clone()),
        UpdateOutcome::Accepted => ApplyLedgerVerdict::Silent,
        #[cfg(any(unix, test))]
        UpdateOutcome::InstalledNeedsRelaunch { .. } => ApplyLedgerVerdict::Silent,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::{
        ApplyLedgerVerdict, ApplyPostureEnv, UPDATE_DIDNT_INSTALL, UPDATE_WAITS_FOR_YOU,
        apply_ledger_verdict, debug_seamless_reexec_armed,
    };
    use crate::App;
    use crate::WindowId;
    use crate::native_app::{AppViewState, UpdateOutcome};
    use crate::native_updater_service::CheckStart;

    /// Verify `debug_seamless_reexec_armed` does not deadlock on first initialization.
    #[test]
    fn the_reexec_seam_reads_the_environment_and_never_itself() {
        let armed = debug_seamless_reexec_armed();
        assert_eq!(
            armed,
            aterm_types::control_socket::env_flag_engaged(
                std::env::var_os("ATERM_DEBUG_SEAMLESS_REEXEC")
                    .map(|v| v.to_string_lossy().into_owned())
                    .as_deref(),
            ),
            "the seam is armed exactly when its variable is ENGAGED (non-empty and not \
             \"0\") — the one flag rule every ATERM_NO_* / QA-seam reader shares, so an \
             inherited empty variable can never arm it"
        );
    }

    /// Only a ready `Staged` row applies on press; other rows and un-staged clicks open `/updates`.
    #[test]
    fn the_update_row_and_menu_apply_only_when_staged_and_ready() {
        let on_software_update = |app: &App| {
            let (_, view) = app
                .active_native_view(WindowId(0))
                .expect("native Settings tab");
            matches!(
                app.native_runtime.view_state(view),
                Some(AppViewState::Settings(state))
                    if state.route == crate::native_settings::SettingsRoute::SoftwareUpdate
            )
        };
        let mut app = App::headless_for_test();
        let id = app.post_update_row(
            crate::update_words::progress(
                &aterm_update::Progress::Staged {
                    version: "9.9.9".into(),
                    build: 1,
                },
                None,
                "",
            ),
            "9.9.9",
            crate::messages_host::FlowPhase::Staged { build: 1 },
        );
        assert_eq!(app.staged_update_row(), Some(id));
        app.perform_intent(
            WindowId(0),
            id,
            aterm_messages::Intent::ApplyUpdate { build: 1 },
        );
        assert!(on_software_update(&app), "the row press");

        let mut app = App::headless_for_test();
        assert!(
            !app.staged_update_ready(),
            "test env must not have a strictly-newer staged build"
        );
        app.apply_update_or_details();
        assert!(on_software_update(&app), "the menu item");
    }

    /// Enabling serious mode mid-update zeroes the upgrade rim's motion while keeping the rim active.
    #[test]
    fn serious_mode_mid_update_stills_the_rim_and_keeps_it() {
        let mut app = App::headless_for_test();
        app.system_reduce_motion = false;
        app.serious_mode = false;
        app.spawn_upgrade_surge(41);
        let at = std::time::Instant::now() + std::time::Duration::from_secs(1);
        let frame = std::time::Duration::from_millis(100);
        let moving = |app: &App| {
            let rim = app.level_up.as_ref().expect("the rim is up");
            rim.fingerprint(at) != rim.fingerprint(at + frame)
        };
        assert!(moving(&app), "PRECONDITION: the rim animates");
        assert!(app.set_serious_mode(true));
        assert!(!moving(&app), "serious mode keeps the rim, still");
    }

    /// Verify row wording and upgrade rim lifecycle across begin, reader park, refusal, and outcome.
    #[test]
    fn apply_begins_charges_the_rim_and_a_refusal_restores_the_row() {
        use crate::messages_host::FlowPhase;
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = App::headless_for_test();
        let rim = |app: &App| app.level_up.is_some();
        let phase = |app: &App| app.live_update_flow().map(|flow| flow.phase);
        let staged_phase = Some(FlowPhase::Staged { build: 41 });
        let staged = aterm_update::Progress::Staged {
            version: "9.9.9".into(),
            build: 41,
        };
        let post_staged = |app: &mut App| {
            app.post_update_row(
                crate::update_words::progress(&staged, None, ""),
                "9.9.9",
                FlowPhase::Staged { build: 41 },
            );
        };
        post_staged(&mut app);
        let ready = app.update_row_text();
        assert_eq!(phase(&app), staged_phase);

        app.begin_update_installing(false);
        assert!(!rim(&app), "no rim over a terminal that still echoes");
        app.charge_upgrade_rim_at_park(41);
        assert!(rim(&app), "the readers parked: the rim explains the freeze");
        assert_eq!(phase(&app), Some(FlowPhase::Installing));
        assert_eq!(
            app.messages.live_rows().count(),
            1,
            "the row was rewritten, not added"
        );
        for _ in 0..2 {
            app.retire_update_installing();
            assert!(!rim(&app));
            assert_eq!(phase(&app), staged_phase);
            assert_eq!(
                app.update_row_text(),
                ready,
                "the words go back (idempotent)"
            );
        }

        app.clear_messages_for_test();
        app.begin_update_installing(false);
        assert_eq!(
            app.messages.live_rows().count(),
            0,
            "the automatic lane never adds a row"
        );
        app.charge_upgrade_rim_at_park(41);
        assert!(rim(&app), "…but the rim still charges at the park");
        app.retire_update_installing();
        assert!(!rim(&app));
        app.begin_update_installing(true);
        assert_eq!(
            phase(&app),
            Some(FlowPhase::Installing),
            "an explicit apply adds the row"
        );
        assert_eq!(
            app.message_band_rows, 1,
            "…and commits it at once, before the carry reads the window"
        );
        app.retire_update_installing();
        assert_eq!(app.messages.live_rows().count(), 0);
        assert_eq!(
            app.message_band_rows, 1,
            "a refusal retires the words without a re-grid of its own"
        );

        app.clear_messages_for_test();
        post_staged(&mut app);
        app.begin_update_installing(false);
        app.charge_upgrade_rim_at_park(41);
        app.note_update_outcome(crate::update_words::failed(
            super::UPDATE_WAITS_FOR_YOU,
            App::UNSAVED_NATIVE_WORK_BLOCKS_APPLY,
            true,
        ));
        assert!(!rim(&app));
        let outcome = app.update_row_text();
        app.retire_update_installing();
        assert_eq!(
            app.update_row_text(),
            outcome,
            "a row an outcome moved on is not retired"
        );
        assert!(app.settle_messages(std::time::Instant::now() + aterm_messages::HOLD_WARN));
        assert_eq!(
            app.update_row_text(),
            ready,
            "the ready row is back, in place"
        );
        assert_eq!(app.messages.live_rows().count(), 1);
    }

    #[test]
    fn selectable_update_menu_click_surfaces_dirty_native_preflight_block() {
        let dir =
            std::env::temp_dir().join(format!("aterm-update-menu-dirty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("draft.md");
        std::fs::write(&path, "draft\n").unwrap();
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

    /// Stage `build` through the check reducer along with the current ledger's `apply_lane` report.
    #[cfg(target_os = "macos")]
    fn stage_build_with_ledger(app: &mut App, build: u64) {
        let current_build = app.native_updater_service.snapshot().current_build;
        let base = aterm_update::status(current_build)
            .map(crate::app_native::durable_update_status)
            .expect("the scratch staging root yields a status under test");
        let status = crate::native_updater_service::DurableUpdateStatus {
            linux_host: false,
            linux: None,
            enabled: true,
            staged_build: Some(build),
            staged_version: Some(format!("1.0.{build}")),
            staged_commit: Some("0123456789abcdef0123456789abcdef01234567".to_string()),
            staged_dmg_sha256: Some("ab".repeat(32)),
            changelog: None,
            outcome: "staged".to_string(),
            installable: Some(true),
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
        });
    }

    /// When a sibling stages a replacement build while an apply attempt runs, a returned
    /// failure is attributed to the attempted build rather than the replacement stage.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_returned_failure_is_not_worded_for_the_stage_that_replaced_it() {
        use crate::messages_host::FlowPhase;
        use crate::native_updater_service::{
            ApplyDecision, ApplyPreflightStart, ClosePreflight, ReturnedApplyDisposition,
            ReturnedApplyFacts,
        };
        use crate::update_words::ApplyPosture;
        let _ledger = super::hold_update_ledger_for_test();
        let message = "overlap handoff failed safely: candidate exited before its proof; the \
                       download on disk has since changed";
        let failed = || UpdateOutcome::Failed {
            message: message.to_string(),
        };
        for decision in [false, true] {
            for source in ["manual handoff", "automatic handoff"] {
                let case = format!("{source} decision={decision}");
                let mut app = App::headless_for_test();
                let running = app.native_updater_service.snapshot().current_build;
                let tried = running + 77_001;
                let replacement = tried + 1;
                stage_build_with_ledger(&mut app, tried);
                app.clear_messages_for_test();
                app.ensure_staged_decision_in(tried, ApplyPosture::ManualByConfig);
                assert!(
                    app.staged_update_row().is_some(),
                    "{case}: PRECONDITION: its ready row"
                );
                app.begin_update_installing(!super::source_is_automatic(source));
                let ApplyPreflightStart::Inspect(preflight) =
                    app.native_updater_service.begin_apply_preflight()
                else {
                    panic!("the stage admits preflight");
                };
                let ApplyDecision::Execute(command) = app
                    .native_updater_service
                    .finish_apply_preflight(preflight, ClosePreflight::Ready)
                else {
                    panic!("preflight mints the attempt");
                };
                let attempt = command.attempt();
                command.execute(|| ());
                assert_eq!(
                    app.native_updater_service.finish_returned_apply(
                        &attempt,
                        ReturnedApplyFacts::new(
                            true,
                            Some(replacement),
                            Some(attempt.target_commit()),
                            Some(attempt.target_dmg_sha256()),
                            None,
                        ),
                        "candidate exited before its proof",
                    ),
                    ReturnedApplyDisposition::Retired
                );
                stage_build_with_ledger(&mut app, replacement);
                if !decision {
                    arm_intent(&mut app, replacement);
                }
                app.retire_update_installing();
                assert_eq!(
                    app.live_update_flow().map(|flow| flow.phase),
                    Some(FlowPhase::Staged { build: tried }),
                    "{case}: PRECONDITION: the tried build's ready row is back"
                );
                if decision {
                    app.record_apply_outcome_for_target_in_ledger(&failed(), tried);
                    app.say_superseded_apply_failure(
                        source,
                        message,
                        tried,
                        replacement,
                        ApplyPosture::ManualByConfig,
                    );
                } else {
                    app.surface_update_apply_outcome_for_target(source, failed(), false, tried);
                }
                let tried_version = format!("v1.0.{tried}");
                let new_version = format!("v1.0.{replacement}");
                for l in app.messages.live_rows() {
                    let said = format!("{} {}", l.msg.title, l.msg.detail.join(" "));
                    assert!(!said.contains(&tried_version), "{case}: stale row: {said}");
                    if l.msg.key.as_deref() == Some(crate::update_words::KEY_OUTCOME) {
                        assert!(!said.contains(&new_version), "{case}: {said}");
                        assert!(
                            !l.msg
                                .actions
                                .iter()
                                .any(|a| matches!(a, aterm_messages::Intent::ApplyUpdate { .. })),
                            "{case}: no press on the failure for bytes nobody tried"
                        );
                    }
                }
                if let Some(said) = app.update_record_text() {
                    assert!(!said.contains(&new_version), "{case}: {said}");
                }
                let told = if super::source_is_automatic(source) {
                    app.update_record_text()
                } else {
                    app.messages
                        .live_by_key(crate::update_words::KEY_OUTCOME)
                        .map(|l| format!("{} — {}", l.msg.title, l.msg.detail.join("; ")))
                };
                assert!(told.is_some(), "{case}: the failure is said");
                let ready = app
                    .live_update_flow()
                    .filter(|flow| flow.phase == FlowPhase::Staged { build: replacement })
                    .and_then(|flow| app.messages.live(flow.id))
                    .map(|l| (l.msg.title.clone(), l.msg.actions.clone()));
                if decision {
                    let (title, actions) = ready.expect("the new stage's decision is raised");
                    assert!(title.contains(&new_version), "{case}: {title}");
                    assert!(
                        actions
                            .contains(&aterm_messages::Intent::ApplyUpdate { build: replacement }),
                        "{case}: its own press"
                    );
                } else {
                    assert_eq!(ready, None, "{case}");
                }
            }
        }
        let mut app = App::headless_for_test();
        let running = app.native_updater_service.snapshot().current_build;
        let tried = running + 77_101;
        stage_build_with_ledger(&mut app, tried);
        app.clear_messages_for_test();
        app.surface_update_apply_outcome_for_target("manual handoff", failed(), false, tried);
        let row = app.update_row_text().expect("a stopped lane is a row");
        assert!(row.contains(&format!("v1.0.{tried}")), "{row}");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn returned_failure_ledger_conforms_to_attempt_target_after_stage_replacement() {
        use crate::native_updater_service::{
            ApplyDecision, ApplyPreflightStart, ClosePreflight, ReturnedApplyDisposition,
            ReturnedApplyFacts,
        };
        let _ledger = super::hold_update_ledger_for_test();
        let model = aterm_models_update::native_update_failure_target_model();
        for replaced in [false, true] {
            let mut app = App::headless_for_test();
            let running = app.native_updater_service.snapshot().current_build;
            let original = running + 65_101 + u64::from(replaced) * 2;
            let replacement = original + 1;
            stage_build_with_ledger(&mut app, original);
            let ApplyPreflightStart::Inspect(preflight) =
                app.native_updater_service.begin_apply_preflight()
            else {
                panic!("the original stage must admit preflight");
            };
            let ApplyDecision::Execute(command) = app
                .native_updater_service
                .finish_apply_preflight(preflight, ClosePreflight::Ready)
            else {
                panic!("preflight must mint the original attempt");
            };
            let attempt = command.attempt();
            command.execute(|| ());
            let disposition = app.native_updater_service.finish_returned_apply(
                &attempt,
                ReturnedApplyFacts::new(
                    true,
                    Some(if replaced { replacement } else { original }),
                    Some(attempt.target_commit()),
                    Some(attempt.target_dmg_sha256()),
                    None,
                ),
                "the original candidate did not start",
            );
            let mut before = model.init_state();
            if replaced {
                assert_eq!(disposition, ReturnedApplyDisposition::Retired);
                stage_build_with_ledger(&mut app, replacement);
                before = model.successors("ReplaceStage", &before).remove(0);
            } else {
                assert_eq!(disposition, ReturnedApplyDisposition::Rearmed);
            }
            assert_eq!(
                app.native_updater_service
                    .snapshot()
                    .staged
                    .as_ref()
                    .unwrap()
                    .build,
                if replaced { replacement } else { original },
            );
            app.surface_update_apply_outcome_for_target(
                "manual handoff",
                UpdateOutcome::Failed {
                    message: "the original candidate did not start".to_string(),
                },
                false,
                attempt.target_build(),
            );
            let report = aterm_update::apply_lane_report(running).expect("real ledger is readable");
            assert_eq!(report.last_failure_target_build, original);
            assert!(report.failures_for_target > 0);
            let mut after = before.clone();
            after.insert(
                "charged",
                if report.last_failure_target_build == original {
                    1
                } else {
                    2
                },
            );
            assert_eq!(model.successors("Publish", &before), vec![after.clone()]);
            assert!(model.check_invariant("FailureBelongsToAttempt", &after));
            if replaced {
                assert!(
                    app.apply_trouble_for(replacement).is_none(),
                    "new bytes inherit no failure"
                );
                app.record_apply_outcome_in_ledger(&UpdateOutcome::Failed {
                    message: "historical inferred target".to_string(),
                });
                let historical = aterm_update::apply_lane_report(running).unwrap();
                assert_eq!(historical.last_failure_target_build, replacement);
                after.insert("charged", 2);
                assert!(!model.successors("Publish", &before).contains(&after));
                assert!(!model.check_invariant("FailureBelongsToAttempt", &after));
            }
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn installed_and_deferred_outcomes_only_promise_a_retained_automatic_retry() {
        let _ledger = super::hold_update_ledger_for_test();
        for (automatic, armed) in [(false, true), (true, false), (true, true)] {
            let mut app = App::headless_for_test();
            let build = app.native_updater_service.snapshot().current_build + 1;
            stage_build_with_ledger(&mut app, build);
            app.config.update = Some(crate::app_config::UpdateConfig {
                auto_apply: Some(automatic),
                ..app.config.update.clone().unwrap_or_default()
            });
            if armed {
                arm_intent(&mut app, build);
            }
            app.react_to_update_apply_outcome(
                "manual",
                UpdateOutcome::InstalledNeedsRelaunch {
                    build,
                    message: App::INSTALLED_ACTIVATES_IN_PLACE.to_string(),
                },
                false,
            );
            let scheduled = automatic && armed;
            assert_eq!(
                app.update_row_text().is_some(),
                !scheduled,
                "no row for a retry that happens by itself"
            );

            app.react_to_update_apply_outcome(
                "manual",
                UpdateOutcome::Deferred {
                    reason: "typing gap".to_string(),
                },
                false,
            );
            assert_eq!(
                app.update_row_text().is_some(),
                !scheduled,
                "no row for a retry that happens by itself"
            );
        }
    }

    /// Verify `apply_posture_with` precedence: unavailable handoff > policy > screenshot
    /// seam > per-build stand-down latch > automatic.
    #[test]
    fn apply_posture_names_config_env_latch_and_disabled_handoff_in_that_order() {
        use crate::app_config::AutoApplySetting;
        use crate::app_update_handoff::HandoffUnavailable;
        use crate::update_apply_trouble::ApplyRetry;
        use crate::update_words::ApplyPosture as P;
        let mut app = App::headless_for_test();
        assert!(
            crate::app_config::update_auto_apply(&app.config)
                && !App::relaunch_nudge_seam_suppresses_auto_apply()
                && crate::cli::launch_flags().control_sock.is_none(),
            "PRECONDITION: no update veto may be set in the test environment"
        );
        let build = stage_one_build(&mut app);
        let latch = |build: u64, retry_at: Option<std::time::Instant>| crate::AutoApplyManualOnly {
            build,
            dmg_sha256: [0xab; 32],
            activation: false,
            retry_at,
        };
        let later = Some(std::time::Instant::now() + std::time::Duration::from_secs(600));
        let clean = ApplyPostureEnv {
            handoff_unavailable: None,
            auto_apply: AutoApplySetting::On,
            nudge_seam: false,
        };
        assert_eq!(
            app.apply_posture_for(build),
            P::HandoffDisabled {
                why: HandoffUnavailable::Headless,
                veto: None,
                image_kept: false,
            },
            "a headless App has no seamless lane, and the bar says so"
        );
        assert_eq!(
            app.apply_posture_with(build, clean),
            P::Automatic,
            "the default policy, with the lane available"
        );

        app.auto_apply_manual_only = Some(latch(build, later));
        assert_eq!(
            app.apply_posture_with(build, clean),
            P::ManualOnlyLatched { lapses: true }
        );
        assert_eq!(
            app.apply_retry_for(Some(build)),
            ApplyRetry::Scheduled,
            "…which is the Scheduled row: the same fact, on the row"
        );
        app.auto_apply_manual_only = Some(latch(build, None));
        assert_eq!(
            app.apply_posture_with(build, clean),
            P::ManualOnlyLatched { lapses: false }
        );
        assert_eq!(app.apply_retry_for(Some(build)), ApplyRetry::ManualOnly);
        app.auto_apply_manual_only = Some(latch(build + 1, None));
        assert_eq!(
            app.apply_posture_with(build, clean),
            P::Automatic,
            "another build's latch is not this build's"
        );

        app.auto_apply_manual_only = Some(latch(build, later));
        assert_eq!(
            app.apply_posture_with(
                build,
                ApplyPostureEnv {
                    auto_apply: AutoApplySetting::OffByConfig,
                    ..clean
                }
            ),
            P::ManualByConfig
        );
        assert_eq!(
            app.apply_posture_with(
                build,
                ApplyPostureEnv {
                    nudge_seam: true,
                    ..clean
                }
            ),
            P::VetoedByEnv {
                var: "ATERM_DEBUG_RELAUNCH_NUDGE"
            }
        );
        for why in HandoffUnavailable::ALL {
            assert_eq!(
                app.apply_posture_with(
                    build,
                    ApplyPostureEnv {
                        handoff_unavailable: Some(why),
                        auto_apply: AutoApplySetting::OffByConfig,
                        nudge_seam: true,
                    }
                ),
                P::HandoffDisabled {
                    why,
                    veto: Some(crate::update_words::AutoApplyVeto::Config),
                    image_kept: false,
                }
            );
        }
        assert_eq!(
            app.apply_posture_with(
                build,
                ApplyPostureEnv {
                    handoff_unavailable: Some(HandoffUnavailable::Headless),
                    auto_apply: AutoApplySetting::On,
                    nudge_seam: true,
                }
            ),
            P::HandoffDisabled {
                why: HandoffUnavailable::Headless,
                veto: Some(crate::update_words::AutoApplyVeto::Env {
                    var: "ATERM_DEBUG_RELAUNCH_NUDGE"
                }),
                image_kept: false,
            }
        );
        assert_eq!(
            app.apply_posture_with(
                build,
                ApplyPostureEnv {
                    handoff_unavailable: Some(HandoffUnavailable::Headless),
                    ..clean
                }
            ),
            P::HandoffDisabled {
                why: HandoffUnavailable::Headless,
                veto: None,
                image_kept: false,
            }
        );

        app.auto_apply_manual_only = None;
        app.config.update = Some(crate::app_config::UpdateConfig {
            auto_apply: Some(false),
            ..app.config.update.clone().unwrap_or_default()
        });
        let observed = ApplyPostureEnv::observe(&app);
        assert_eq!(observed.auto_apply, AutoApplySetting::OffByConfig);
        assert_eq!(
            app.apply_posture_with(
                build,
                ApplyPostureEnv {
                    handoff_unavailable: None,
                    ..observed
                }
            ),
            P::ManualByConfig
        );
        assert_eq!(
            app.apply_posture_for(build),
            P::HandoffDisabled {
                why: HandoffUnavailable::Headless,
                veto: Some(crate::update_words::AutoApplyVeto::Config),
                image_kept: false,
            }
        );
    }

    /// `note_update_progress` records `apply_posture_for` on a `Staged` report.
    #[test]
    fn a_staged_report_paints_the_bar_with_the_posture_the_app_computed() {
        use crate::app_update_handoff::HandoffUnavailable;
        use crate::update_words::{ApplyPosture, staged_detail};
        let mut app = App::headless_for_test();
        assert!(
            crate::app_config::update_auto_apply(&app.config)
                && !App::relaunch_nudge_seam_suppresses_auto_apply()
                && crate::cli::launch_flags().control_sock.is_none(),
            "PRECONDITION: no update veto may be set in the test environment"
        );
        let build = stage_one_build(&mut app);
        let staged = aterm_update::Progress::Staged {
            version: "9.9.9".to_string(),
            build,
        };
        let staged_record = |app: &App| {
            app.messages
                .log()
                .records()
                .rev()
                .find(|r| r.key.as_deref() == Some(crate::update_words::KEY_PROGRESS))
                .filter(|r| r.retired() == Some(&aterm_messages::Retired::Recorded))
                .map(|r| r.detail.join("; "))
                .expect("the staged report is recorded")
        };
        app.note_update_progress(&staged);
        assert!(
            app.messages
                .live_by_key(crate::update_words::KEY_PROGRESS)
                .is_none(),
            "a staged build nothing can press is not a band row"
        );
        let headless = ApplyPosture::HandoffDisabled {
            why: HandoffUnavailable::Headless,
            veto: None,
            image_kept: false,
        };
        assert_eq!(staged_record(&app), staged_detail(headless));
        assert_eq!(app.message_band_rows, 0, "…and no row is reserved");

        let latch = |retry_at| crate::AutoApplyManualOnly {
            build,
            dmg_sha256: [0xab; 32],
            activation: false,
            retry_at,
        };
        app.auto_apply_manual_only = Some(latch(None));
        assert_eq!(app.apply_posture_for(build), headless);
        let before = app.messages.revision();
        app.restate_staged_bar_posture(build);
        assert_eq!(app.messages.revision(), before, "the words did not change");
    }

    /// End-to-end check that `surface_update_apply_outcome` writes the ledger, updates
    /// the reducer immediately, and projects the failure into the Version menu, palette,
    /// and Settings page.
    #[cfg(target_os = "macos")]
    #[test]
    fn one_real_failed_apply_reaches_every_standing_surface_at_once() {
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = App::headless_for_test();
        let current_build = app.native_updater_service.snapshot().current_build;
        let build = current_build + 4_101;
        stage_build_with_ledger(&mut app, build);
        arm_intent(&mut app, build);

        assert!(
            app.apply_trouble_for(build).is_none(),
            "a stage nothing has attempted carries no trouble"
        );

        app.surface_update_apply_outcome(
            "automatic handoff",
            UpdateOutcome::Failed {
                message: "overlap handoff failed safely: handoff proof ended ChildDied".to_string(),
            },
            false,
        );
        use crate::update_apply_trouble::{ApplyRetry, ApplyTrouble};

        let message = "overlap handoff failed safely: handoff proof ended ChildDied";
        let trouble = app
            .apply_trouble_for(build)
            .expect("the failure that just happened is standing trouble for this build");
        assert_eq!(
            Some(&trouble),
            ApplyTrouble::new(1, message, ApplyRetry::Scheduled).as_ref(),
            "one attempt, its cause, and the armed lane's retry"
        );
        assert_ne!(
            crate::menu::staged_apply_label("\u{2191}", build, "9.9.9", Some(&trouble), false),
            crate::menu::staged_apply_label("\u{2191}", build, "9.9.9", None, false),
            "the menu item carries the trouble"
        );

        let live = app.palette_live();
        assert_eq!(
            live.staged.as_ref().map(|(b, _)| *b),
            Some(build),
            "the ledger write republished the staged nudge"
        );
        assert_eq!(live.staged_trouble.as_ref(), Some(&trouble));

        assert_eq!(
            app.update_snapshot(false)
                .projection()
                .apply_trouble
                .as_ref(),
            Some(&trouble),
            "the failure reached the page at once"
        );

        app.auto_apply_intent = None;
        app.auto_apply_manual_only = None;
        let latched = ApplyTrouble::new(1, message, ApplyRetry::ManualOnly);
        assert_eq!(app.apply_trouble_for(build), latched);
        assert_eq!(
            app.update_snapshot(false).projection().apply_trouble,
            latched,
            "a lane that has stopped must say so on the page too"
        );
    }

    /// Verify a fresh `App` imports prior apply failure count and reason from the ledger via `durable_update_status`.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_check_that_lands_on_a_ledger_reads_the_reason_and_not_only_the_count() {
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = App::headless_for_test();
        let current_build = app.native_updater_service.snapshot().current_build;
        let staged = current_build + 6_301;

        let recorded = aterm_update::record_apply_failure(
            current_build,
            staged,
            "overlap handoff failed safely: handoff proof ended AdoptionMismatch",
        )
        .expect("the scratch staging root is resolvable under test");
        assert_eq!(recorded.failures_for_target, 1);

        stage_build_with_ledger(&mut app, staged);
        app.auto_apply_intent = None;
        app.auto_apply_manual_only = None;

        use crate::update_apply_trouble::{ApplyRetry, ApplyTrouble};
        let expected = ApplyTrouble::new(
            1,
            "overlap handoff failed safely: handoff proof ended AdoptionMismatch",
            ApplyRetry::ManualOnly,
        );
        assert_eq!(
            app.apply_trouble_for(staged),
            expected,
            "the CAUSE came off the disk, and so did the fact that nothing is scheduled"
        );
        assert_eq!(
            app.update_snapshot(false).projection().apply_trouble,
            expected,
            "…and the page reads the same"
        );
    }

    /// Staging a newer build does not inherit `apply_trouble_for` from the superseded build.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_newer_stage_starts_clean_even_though_the_running_build_has_not_changed() {
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = App::headless_for_test();
        let current_build = app.native_updater_service.snapshot().current_build;
        let superseded = current_build + 5_201;
        let fresh = superseded + 1;

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
        assert!(
            app.update_snapshot(false)
                .projection()
                .apply_trouble
                .is_none(),
            "…and the page agrees with the menu"
        );

        app.surface_update_apply_outcome(
            "automatic handoff",
            UpdateOutcome::Failed {
                message: "overlap handoff failed safely: handoff proof ended TimedOut".to_string(),
            },
            false,
        );
        let trouble = app.apply_trouble_for(fresh).expect("its own failure");
        assert_eq!(
            Some(trouble.with_retry_in(None)),
            crate::update_apply_trouble::ApplyTrouble::new(
                1,
                "overlap handoff failed safely: handoff proof ended TimedOut",
                app.apply_retry_for(Some(fresh)),
            ),
            "the count restarted with the artifact, and the CAUSE moved with it"
        );
    }

    /// `apply_trouble_for` returns `None` when the target build is not newer than `current_build`.
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

    pub(crate) fn stage_one_build(app: &mut App) -> u64 {
        let current_build = app.native_updater_service.snapshot().current_build;
        let build = current_build + 1;
        let CheckStart::Start(ticket) = app.native_updater_service.request_check() else {
            panic!("a fresh service must start exactly one check");
        };
        assert_eq!(
            app.native_updater_service.finish_check(
                ticket,
                crate::native_updater_service::DurableUpdateStatus {
                    linux_host: false,
                    linux: None,
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
                    installable: Some(true),
                    channel_unreadable: false,
                    checked_at: None,
                },
            ),
            crate::native_updater_service::CheckCompletion::Reduced,
            "PRECONDITION: the check must reduce, or nothing is staged and the \
             predicate under test is never reached"
        );
        build
    }

    #[test]
    fn retry_and_latch_posture_require_the_exact_current_artifact() {
        use crate::app_config::AutoApplySetting;
        use crate::update_words::ApplyPosture;
        let mut app = App::headless_for_test();
        let build = stage_one_build(&mut app);
        let later = std::time::Instant::now() + std::time::Duration::from_secs(600);
        let available = ApplyPostureEnv {
            handoff_unavailable: None,
            auto_apply: AutoApplySetting::On,
            nudge_seam: false,
        };
        for digest in [[0xab; 32], [0xcd; 32]] {
            let exact = digest == [0xab; 32];
            app.auto_apply_manual_only = None;
            app.auto_apply_intent = Some(crate::AutoApplyIntent {
                build,
                dmg_sha256: digest,
                retry_at: later,
                attempts: 0,
            });
            assert_eq!(app.automatic_apply_retry_scheduled(build), exact);
            assert!(!app.automatic_apply_retry_scheduled(build + 1));
            app.auto_apply_intent = None;
            app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
                build,
                dmg_sha256: digest,
                activation: false,
                retry_at: Some(later),
            });
            assert_eq!(app.automatic_apply_retry_scheduled(build), exact);
            assert_eq!(
                app.apply_posture_with(build, available),
                if exact {
                    ApplyPosture::ManualOnlyLatched { lapses: true }
                } else {
                    ApplyPosture::Automatic
                }
            );
        }
        app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
            build,
            dmg_sha256: [0xab; 32],
            activation: false,
            retry_at: Some(later),
        });
        assert_eq!(
            app.native_updater_service
                .reconcile_durable_stage(true, None, None, None, None),
            crate::native_updater_service::DurableStageDisposition::Retired
        );
        assert!(!app.automatic_apply_retry_scheduled(build));
    }

    /// Scheduled automatic retries post no band row; stopped lanes and manual failures do.
    #[test]
    fn a_scheduled_retry_is_silent_and_a_stopped_lane_names_the_menu() {
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
        let row = |app: &App| app.update_row_text();

        app.auto_apply_intent = None;
        app.auto_apply_manual_only = None;
        app.clear_messages_for_test();
        app.surface_update_apply_outcome("automatic", failed(), false);
        assert!(row(&app).is_some(), "a stopped lane is said");

        app.auto_apply_intent = Some(crate::AutoApplyIntent {
            build,
            dmg_sha256: [0xab; 32],
            retry_at: std::time::Instant::now() + std::time::Duration::from_secs(600),
            attempts: 0,
        });
        app.clear_messages_for_test();
        app.surface_update_apply_outcome("automatic", failed(), false);
        assert_eq!(row(&app), None);

        app.auto_apply_intent = Some(crate::AutoApplyIntent {
            build: build + 7,
            dmg_sha256: [0xab; 32],
            retry_at: std::time::Instant::now() + std::time::Duration::from_secs(600),
            attempts: 0,
        });
        app.clear_messages_for_test();
        app.surface_update_apply_outcome("automatic", failed(), false);
        assert!(row(&app).is_some(), "a stopped lane is said");

        app.auto_apply_intent = None;
        let lapsing = crate::AutoApplyManualOnly {
            build,
            dmg_sha256: [0xab; 32],
            activation: false,
            retry_at: Some(std::time::Instant::now() + std::time::Duration::from_secs(600)),
        };
        app.auto_apply_manual_only = Some(lapsing);
        for cycles in [1_u8, 8] {
            app.auto_apply_physical_retry = Some(crate::AutoOverlapRetry {
                build,
                dmg_sha256: [0xab; 32],
                activation: false,
                cycles,
                last_attempt: std::time::Instant::now(),
            });
            app.clear_messages_for_test();
            app.surface_update_apply_outcome("automatic handoff", failed(), false);
            assert_eq!(
                row(&app),
                None,
                "physical failure {cycles} for this artifact"
            );
        }
        app.auto_apply_physical_retry = None;

        app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
            build,
            dmg_sha256: [0xab; 32],
            activation: false,
            retry_at: None,
        });
        app.clear_messages_for_test();
        app.surface_update_apply_outcome("automatic", failed(), false);
        assert!(row(&app).is_some(), "a stopped lane is said");

        let manual_ticket = crate::native_updater_service::ApplyAttemptTicket::for_test(
            build,
            "0123456789abcdef0123456789abcdef01234567",
            &"ab".repeat(32),
        );
        manual_ticket.make_current_apply_for_test(&mut app.native_updater_service);
        app.auto_apply_physical_retry = Some(crate::AutoOverlapRetry {
            build,
            dmg_sha256: [0xab; 32],
            activation: false,
            cycles: 5,
            last_attempt: std::time::Instant::now() - std::time::Duration::from_secs(600),
        });
        app.auto_apply_manual_only = Some(lapsing);
        app.clear_messages_for_test();
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
        assert_eq!(row(&app), None, "a self-retry takes no row");
        assert!(
            app.update_record_text().is_some(),
            "whoever just asked for the update finds it did not happen, on record"
        );
        app.auto_apply_physical_retry = None;

        app.config.update = Some(crate::app_config::UpdateConfig {
            auto_apply: Some(false),
            ..app.config.update.clone().unwrap_or_default()
        });
        assert!(
            !crate::app_config::update_auto_apply(&app.config),
            "PRECONDITION: the opt-out actually took"
        );
        app.auto_apply_manual_only = Some(lapsing);
        app.clear_messages_for_test();
        app.surface_update_apply_outcome("automatic", failed(), false);
        assert!(row(&app).is_some(), "the opted-out lane is said");
    }

    /// Close preflight names user-actionable blockers (editor/Settings drafts) on the row
    /// and suppresses self-clearing blockers while retry is scheduled.
    #[test]
    fn only_a_blocker_a_person_can_clear_is_named_on_the_row() {
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = App::headless_for_test();
        let blocked = |reasons: &[&str]| UpdateOutcome::Blocked {
            reasons: reasons.iter().map(|r| (*r).to_string()).collect(),
        };
        const SETTINGS: &str = "Review Settings Drafts: 1 Settings view(s) have unsaved text";
        for (reasons, said) in [
            (
                &[App::UNSAVED_NATIVE_WORK_BLOCKS_APPLY][..],
                App::UNSAVED_NATIVE_WORK_BLOCKS_APPLY,
            ),
            (&[SETTINGS][..], App::UNSAVED_SETTINGS_BLOCKS_APPLY),
            (
                &["Checkpoint Drafts: 2 document(s) have uncheckpointed edits"][..],
                App::UNSAVED_NATIVE_WORK_BLOCKS_APPLY,
            ),
            (
                &["Retry: 1 document checkpoint(s) previously failed"][..],
                App::UNSAVED_NATIVE_WORK_BLOCKS_APPLY,
            ),
            (
                &[
                    "Checkpoint Drafts: 2 document(s) have uncheckpointed edits",
                    SETTINGS,
                ][..],
                App::UNSAVED_EDITOR_AND_SETTINGS_BLOCK_APPLY,
            ),
        ] {
            app.clear_messages_for_test();
            app.react_to_update_apply_outcome("automatic", blocked(reasons), false);
            assert_eq!(
                app.update_row_text(),
                Some(format!("{UPDATE_WAITS_FOR_YOU} \u{2014} {said}")),
                "{reasons:?}"
            );
        }
        app.clear_messages_for_test();
        app.react_to_update_apply_outcome(
            "automatic",
            blocked(&[App::RESTORE_IN_FLIGHT_BLOCKS_APPLY]),
            false,
        );
        assert_eq!(app.update_row_text(), None);
        assert_eq!(
            app.update_record_text(),
            Some(format!(
                "{UPDATE_DIDNT_INSTALL} \u{2014} {}",
                crate::update_words::INSTALL_FROM_MENU
            ))
        );
        app.clear_messages_for_test();
        app.note_update_blockers(&[App::RESTORE_IN_FLIGHT_BLOCKS_APPLY.to_string()], true);
        assert_eq!(app.update_row_text(), None);
        assert!(
            super::source_is_automatic("automatic · retrying later")
                && super::source_is_automatic("automatic policy fallback")
                && !super::source_is_automatic("manual handoff")
                && !super::source_is_automatic("control request")
        );
    }

    /// Verify `apply_ledger_verdict` maps `Accepted` and `InstalledNeedsRelaunch` to `Silent`,
    /// `Failed` and `CaptureRefused` to `Failed`, and `Deferred`/`Blocked` to `Refused`.
    #[test]
    fn a_submitted_then_failed_apply_writes_one_streak_increment_and_never_a_clear() {
        use ApplyLedgerVerdict::{Failed, Refused, Silent};
        for (outcome, want) in [
            (UpdateOutcome::Accepted, Silent),
            (
                UpdateOutcome::Failed {
                    message: "ChildDied".to_string(),
                },
                Failed("ChildDied".to_string()),
            ),
            (
                UpdateOutcome::CaptureRefused {
                    message: "the park's capture refused session 3".to_string(),
                },
                Failed("the park's capture refused session 3".to_string()),
            ),
            (
                UpdateOutcome::Blocked {
                    reasons: vec![
                        "Checkpoint Drafts: 1 document(s)".to_string(),
                        "busy".to_string(),
                    ],
                },
                Refused("Checkpoint Drafts: 1 document(s) · busy".to_string()),
            ),
            (
                UpdateOutcome::Deferred {
                    reason: "the terminal is busy".to_string(),
                },
                Refused("the terminal is busy".to_string()),
            ),
            (
                UpdateOutcome::InstalledNeedsRelaunch {
                    build: 7,
                    message: "activating in place".to_string(),
                },
                Silent,
            ),
        ] {
            assert_eq!(apply_ledger_verdict(&outcome), want, "{outcome:?}");
        }
    }

    /// Verify `update_awaited` distinguishes user-initiated and `ctl` checks from background checks.
    #[test]
    fn a_download_is_the_persons_while_their_check_runs() {
        let download = |done: u64| aterm_update::Progress::Downloading {
            version: "0.79.0".into(),
            bytes_done: done,
            bytes_total: 90,
        };
        let mut app = App::headless_for_test();
        assert!(!app.update_awaited());
        app.note_update_progress(&download(45));
        assert_eq!(
            app.update_row_text(),
            None,
            "the background loop's is silent"
        );
        assert!(app.update_quiet.is_some(), "Software Update says it");
        let CheckStart::Start(_) = app.native_updater_service.request_check() else {
            panic!("a check starts");
        };
        assert!(app.update_awaited());
        app.note_update_progress(&download(60));
        assert!(
            app.update_row_text().is_some(),
            "the row is up for the person"
        );
        assert!(
            app.update_quiet.is_none(),
            "and the page's quiet line is gone"
        );
        let mut ctl = App::headless_for_test();
        ctl.note_update_check_asked(true);
        ctl.note_update_check_asked(true);
        ctl.note_update_check_asked(false);
        assert!(ctl.update_awaited(), "one of two still runs");
        ctl.note_update_progress(&download(10));
        assert!(ctl.update_row_text().is_some());
        ctl.note_update_check_asked(false);
        ctl.note_update_check_asked(false);
        assert!(!ctl.update_awaited(), "never below zero");
        ctl.note_update_progress(&download(80));
        assert!(
            ctl.update_row_text().is_some(),
            "the row already up keeps moving to its end"
        );
    }

    /// Quiet background download progress updates an open Software Update Settings view.
    #[test]
    fn the_quiet_download_repaints_an_open_settings_page() {
        let mut app = App::headless_for_test();
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::SoftwareUpdate));
        let (_, view) = app
            .active_native_view(WindowId(0))
            .expect("active Settings view");
        let presentation = |app: &App| match app.native_runtime.view_state(view) {
            Some(AppViewState::Settings(state)) => state.common.presentation_revision,
            _ => panic!("Settings view state"),
        };
        let before = presentation(&app);
        app.note_update_progress(&aterm_update::Progress::Downloading {
            version: "0.92.0".into(),
            bytes_done: 5_400_000,
            bytes_total: 1_200_000_000,
        });
        assert_eq!(app.update_row_text(), None, "silent");
        assert!(presentation(&app) > before, "the open page repaints");
        app.note_update_progress(&aterm_update::Progress::Staged {
            version: "0.92.0".into(),
            build: 41,
        });
        assert!(app.update_quiet.is_none());
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
    }

    /// `ensure_staged_decision_in` raises the ready row at most once per `(build, posture)`.
    #[test]
    fn a_stopped_lane_raises_the_staged_decision_once() {
        use crate::update_words::ApplyPosture;
        let mut app = App::headless_for_test();
        let build = stage_one_build(&mut app);
        app.ensure_staged_decision_in(build + 1, ApplyPosture::ManualByConfig);
        assert_eq!(app.messages.live_rows().count(), 0);
        for posture in [
            ApplyPosture::Automatic,
            ApplyPosture::ManualOnlyLatched { lapses: true },
        ] {
            app.ensure_staged_decision_in(build, posture);
            assert_eq!(
                app.messages.live_rows().count(),
                0,
                "{posture:?}: the lane still acts, so nothing is asked"
            );
        }
        app.ensure_staged_decision_in(build, ApplyPosture::ManualOnlyLatched { lapses: false });
        let id = app.staged_update_row().expect("the decision row");
        app.ensure_staged_decision_in(build, ApplyPosture::ManualOnlyLatched { lapses: false });
        app.ensure_staged_decision_in(build, ApplyPosture::ManualByConfig);
        assert_eq!(app.messages.live_rows().count(), 1, "raised once");
        assert_eq!(app.staged_update_row(), Some(id));
        let row = app.messages.live(id).expect("live");
        assert_eq!(
            row.msg.actions,
            vec![aterm_messages::Intent::ApplyUpdate { build }]
        );
        let lapsed = std::time::Instant::now() + aterm_messages::HOLD_STAGED_MANUAL * 2;
        app.messages.settle(lapsed, true);
        app.sync_messages();
        assert_eq!(app.staged_update_row(), None, "the question lapsed");
        for _ in 0..3 {
            app.ensure_staged_decision_in(build, ApplyPosture::ManualOnlyLatched { lapses: false });
        }
        assert_eq!(
            app.staged_update_row(),
            None,
            "a reconcile never asks again"
        );
        app.ensure_staged_decision_in(build, ApplyPosture::ManualByConfig);
        assert!(
            app.staged_update_row().is_some(),
            "a new posture asks again"
        );
    }

    /// Posting a stopped-lane outcome withdraws any live staged ready row.
    #[test]
    fn a_stopped_lane_is_one_decision_row_never_two() {
        use crate::update_words::ApplyPosture;
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = App::headless_for_test();
        let build = stage_one_build(&mut app);
        let presses = |app: &App| {
            app.messages
                .live_rows()
                .filter(|l| {
                    l.msg
                        .actions
                        .contains(&aterm_messages::Intent::ApplyUpdate { build })
                })
                .count()
        };
        let outcomes = [
            UpdateOutcome::Failed {
                message: "overlap handoff failed safely: handoff proof ended TimedOut".to_string(),
            },
            UpdateOutcome::Blocked {
                reasons: vec!["the handoff helper is not ready".to_string()],
            },
        ];
        for (outcome, posture) in outcomes.into_iter().zip([
            ApplyPosture::ManualOnlyLatched { lapses: false },
            ApplyPosture::ManualByConfig,
        ]) {
            app.clear_messages_for_test();
            app.ensure_staged_decision_in(build, posture);
            assert!(
                app.staged_update_row().is_some(),
                "PRECONDITION: the ready row is up ({posture:?})"
            );
            assert_eq!(presses(&app), 1);
            app.react_to_update_apply_outcome("automatic handoff", outcome, false);
            assert_eq!(
                presses(&app),
                1,
                "one `Install now` on the glass: {:?}",
                app.messages
                    .live_rows()
                    .map(|l| l.msg.title.clone())
                    .collect::<Vec<_>>()
            );
            assert_eq!(app.staged_update_row(), None, "the ready row left");
            let live = app
                .messages
                .live_by_key(crate::update_words::KEY_OUTCOME)
                .expect("the outcome is the decision");
            assert_eq!(live.msg.severity, aterm_messages::Severity::Warn);
            app.sync_messages();
            assert_eq!(app.message_band_rows, 1, "one row for one fact");
        }
    }
    /// When no stage remains, an automatic failure is log-only while a manual press posts `UPDATE_DIDNT_FINISH`.
    #[test]
    fn an_automatic_failure_with_no_stage_is_a_record_and_a_press_is_a_row() {
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let failed = || UpdateOutcome::Failed {
            message: "the staged artifact was retired".to_string(),
        };
        let mut app = App::headless_for_test();
        assert!(app.native_updater_service.snapshot().staged.is_none());
        let before = app.messages.log().len();
        app.react_to_update_apply_outcome("automatic handoff", failed(), false);
        assert_eq!(
            app.update_row_text(),
            None,
            "no row for an unattended attempt"
        );
        assert_eq!(app.messages.log().len(), before + 1, "one record");
        assert!(app.update_record_text().is_some(), "on record");

        let mut pressed = App::headless_for_test();
        pressed.react_to_update_apply_outcome("manual", failed(), false);
        assert!(
            pressed.update_row_text().is_some(),
            "a person's press is told on the glass"
        );
    }

    /// `InstalledNeedsRelaunch` resolves the staged row `Ok`, while `Failed` with no stage withdraws it.
    #[test]
    fn a_gone_artifact_never_echoes_installed_beside_its_failure() {
        use aterm_messages::{LogState, Outcome, Retired};
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let build = 41;
        for (outcome, retired) in [
            (
                UpdateOutcome::InstalledNeedsRelaunch {
                    build,
                    message: "activation pending".to_string(),
                },
                Retired::Resolved(Outcome::Ok),
            ),
            (
                UpdateOutcome::Failed {
                    message: "the staged artifact was retired".to_string(),
                },
                Retired::Withdrawn,
            ),
        ] {
            let mut app = App::headless_for_test();
            assert!(
                app.native_updater_service.snapshot().staged.is_none(),
                "PRECONDITION: no staged build"
            );
            let id = app.post_update_row(
                crate::update_words::staged(
                    "9.9.9",
                    build,
                    Some(crate::update_words::ApplyPosture::ManualByConfig),
                ),
                "9.9.9",
                crate::messages_host::FlowPhase::Staged { build },
            );
            assert!(
                app.staged_update_row().is_some(),
                "PRECONDITION: staged row"
            );
            app.react_to_update_apply_outcome("automatic handoff", outcome.clone(), false);
            assert_eq!(app.staged_update_row(), None, "{outcome:?}: the row left");
            assert_eq!(
                app.messages.log().get(id).map(|r| r.state.clone()),
                Some(LogState::Retired(retired)),
                "{outcome:?}"
            );
        }
    }
}
