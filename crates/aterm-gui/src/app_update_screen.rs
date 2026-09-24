// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `App` glue for the UPDATE FLOW: the ONE-CLICK apply every "update ready" affordance
//! fires ([`App::apply_update_or_details`] — the Version menu's ⬆️ item, the palette's
//! Version row, a press on the staged band row's `Install now` (`App::perform_intent`), the
//! off-macOS tab-strip ↻), the live Version-menu
//! re-sync ([`App::refresh_version_menu`]), and the process updater state projected into
//! the native Settings `/updates` route. (The retired own-rendered modal's glue —
//! `update_screen_exit`/`_refresh`/`_check`, `update_hit_at`, `update_screen_click`,
//! `on_key_update_mode`, `update_input_event` — was DELETED on 2026-09-15 with the card
//! itself; see `update_screen.rs`.)

use crate::App;
use crate::native_app::UpdateOutcome;
use crate::update_screen::UpdateState;

/// Whether an apply outcome's `source` is the AUTOMATIC lane's own — every
/// automatic caller labels itself `automatic…` ("automatic", "automatic handoff",
/// "automatic · retrying later", "automatic policy fallback"); a person's lanes
/// are "manual", "manual handoff" and "control request". Only a retry of the
/// lane's OWN attempt goes unannounced.
fn source_is_automatic(source: &str) -> bool {
    source.starts_with("automatic")
}

/// Serializes tests that record failures or read standing failure facts in the
/// process-wide scratch update ledger.
///
/// `App::headless_for_test` points `ATERM_UPDATE_ROOT` at ONE scratch root per test
/// process (so the unit suite cannot write the developer's real `health.toml`), and
/// the ledger's standing slots — `last_apply_failure_target_build` above all — are
/// single-valued. A test that records a failure for its own build number and then
/// asks the reducer to read it back is therefore racing every sibling that records
/// one for a different build: observed, not theorised, on the first parallel run.
/// The build numbers below are already unique per test, which keeps the COUNTS
/// independent; this keeps the standing slots independent too.
/// Every failure writer must participate, including tests that assert only the
/// in-memory retry policy. The ledger's own file lock protects each individual
/// write; this guard also protects a test's later read from another test's write.
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

#[cfg(test)]
impl App {
    /// The update lane's NEWEST live row's words, `"<title> — <detail>"`, or
    /// `None` with no update row up — for the tests that used to read the
    /// floating pill and then the one bar. The center can hold the progress
    /// row and an outcome row side by side (design §5.3); the newest — the
    /// highest id — is the one an outcome test asks about.
    pub(crate) fn update_row_text(&self) -> Option<String> {
        self.messages
            .live_rows()
            .filter(|l| l.msg.tag == aterm_messages::tags::UPDATE)
            .max_by_key(|l| l.id)
            .map(|l| format!("{} — {}", l.msg.title, l.msg.detail.join("; ")))
    }

    /// The update lane's NEWEST RECORDED outcome's words, `"<title> — <detail>"`
    /// — ruling 143: an outcome the lane answers by itself is a record, never a
    /// band row, so the tests that read its words read them off the log.
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

    /// The update lane's PROGRESS row's detail, its lines joined back with
    /// `; ` (the staged sentence is split at those joints when it exceeds the
    /// line cap); `None` with no progress row up.
    pub(crate) fn update_row_detail(&self) -> Option<String> {
        self.messages
            .live_by_key(crate::update_words::KEY_PROGRESS)
            .map(|l| l.msg.detail.join("; "))
    }

    /// Start the center over: an empty ring, nothing live — what a test that
    /// wants a clean lane does between cases.
    pub(crate) fn clear_messages_for_test(&mut self) {
        self.messages = aterm_messages::MessageCenter::new(
            aterm_messages::MessageLog::empty(),
            std::time::Instant::now(),
        );
        self.update_row_before_install = None;
        // A fresh center mints ids from 1 again: a remembered flow id would
        // name whatever row the next test posts under it.
        self.update_flow = None;
        self.staged_decision_raised = None;
        // The committed count mirrors the center's; a fresh center commits
        // nothing, so the geometry gives its rows back at once.
        if self.message_band_rows != 0 {
            self.message_band_rows = 0;
            self.regrid_for_chrome_rows();
        }
        self.sync_messages();
    }
}

/// `true` while the `ATERM_DEBUG_SEAMLESS_REEXEC` QA seam is armed — and it SAYS
/// SO, once per process, the first time anything asks. A DEVELOPMENT SEAM
/// (`aterm_types::dev_seam!`, 2026-09-23): a shipped binary never reads it, so it is
/// always `false` there.
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
        //
        // Value semantics via the ONE shared flag rule (`env_flag_engaged`):
        // unset, EMPTY and "0" do NOT arm the seam. `is_some()` here armed it on
        // a present-but-empty variable — the 2026-09-01 empty-inherited-var
        // species — and an armed seam re-execs THIS binary and reports the apply
        // as done while the staged build never lands (2026-09-14 audit).
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

/// The update bar's words for a build that is already installed on disk where
/// nothing will switch to it by itself: the reducer imports it as an ACTIVATION
/// stage, and policy requires a person's request. (With a retry scheduled the
/// flow row says the lane is still on it, and no outcome is posted.)
pub(crate) const UPDATE_INSTALLED_TITLE: &str = "Update installed";
pub(crate) const UPDATE_INSTALLED_DETAIL: &str = "finish it from the Version menu";
/// An update the lane has stopped trying by itself: the person's next step is
/// the Version menu's "Install aterm vX now" — the row's `Install now`.
pub(crate) const UPDATE_DIDNT_INSTALL: &str = "Update didn't install";
/// An attempt that failed with no staged build left to retry (main's words,
/// ruling 68): a failure row with the details page.
pub(crate) const UPDATE_DIDNT_FINISH: &str = "Update didn't finish";
/// The install waits on work only the person can save or close (the close
/// preflight's blocker, painted beneath it — ruling 143's editor block, said
/// on its own row when no flow row is up to say it).
pub(crate) const UPDATE_WAITS_FOR_YOU: &str = "Update waits for you";

/// The three answers about this process [`App::apply_posture_for`] folds in,
/// read ONCE at the call site so the pure half ([`App::apply_posture_with`]) is
/// testable without touching process environment (the tests in this crate
/// assert those variables' ABSENCE).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ApplyPostureEnv {
    /// Why the in-session handoff cannot run in this process, if it cannot —
    /// [`App::seamless_handoff_unavailable`], the SAME reading the apply gate
    /// takes for `seamless_capable`. With a reason here the apply is REFUSED
    /// while any terminal is open (`native_update_admission::classify` admits
    /// the cold lane only with zero live PTYs), so no promise about WHEN it
    /// applies is true.
    pub(crate) handoff_unavailable: Option<crate::app_update_handoff::HandoffUnavailable>,
    /// `[update] auto_apply` (the environment veto that was folded in here is gone,
    /// 2026-09-23).
    pub(crate) auto_apply: crate::app_config::AutoApplySetting,
    /// `$ATERM_DEBUG_RELAUNCH_NUDGE` is set: the screenshot seam vetoes the lane.
    pub(crate) nudge_seam: bool,
}

impl ApplyPostureEnv {
    /// The live answers. Plain reads — `var_os` and two fields of the App
    /// (`seamless_handoff_unavailable`), and the fixed-shape memo in
    /// `relaunch_nudge_seam_suppresses_auto_apply`, whose initializer reads the
    /// ENVIRONMENT — never a memo whose initializer could call its own owner:
    /// that exact shape parked the main thread forever on 2026-08-30.
    pub(crate) fn observe(app: &App) -> Self {
        Self {
            handoff_unavailable: app.seamless_handoff_unavailable(),
            auto_apply: crate::app_config::update_auto_apply_setting(&app.config),
            nudge_seam: App::relaunch_nudge_seam_suppresses_auto_apply(),
        }
    }
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
        UpdateState::from_service(snapshot, checking, retry).with_automatic_checks(
            self.update_checks_running,
            crate::app_config::update_checks_automatic(&self.config),
        )
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
    /// Both must name the current stage's build AND digest: a leftover from a
    /// superseded artifact schedules nothing for different bytes sharing its build.
    /// Automatic apply must also be enabled, including the screenshot-seam veto,
    /// or a lapse would re-arm into a poll that answers `Clear`.
    ///
    /// Extracted from the failure-pill decision in
    /// [`Self::react_to_update_apply_outcome`] so the STANDING surfaces (the Version
    /// menu, the palette's Version row, the Settings page) answer the question the
    /// same way the transient pill does. Two answers to "will this retry?" in one
    /// program is how a user ends up waiting on something that stopped.
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

    /// [`Self::automatic_apply_retry_scheduled`] as the typed value the wording law
    /// reads. `None` — no build on offer — answers `ManualOnly`, the conservative
    /// half: promising a retry that is not scheduled is the one error that costs the
    /// reader real time.
    pub(crate) fn apply_retry_for(
        &self,
        staged_build: Option<u64>,
    ) -> crate::update_apply_trouble::ApplyRetry {
        use crate::update_apply_trouble::ApplyRetry;
        // The historical failure describes the previous attempt. A current
        // environmental block can require repair; a later verified repair can
        // restore scheduling without rewriting that history.
        if staged_build.is_some_and(|build| self.auto_apply_environment_blocked(build)) {
            return ApplyRetry::NeedsPerson;
        }
        match staged_build {
            Some(build) if self.automatic_apply_retry_scheduled(build) => ApplyRetry::Scheduled,
            _ => ApplyRetry::ManualOnly,
        }
    }

    /// HOW the staged `build` will be applied — the band row's finer law
    /// ([`crate::update_words::ApplyPosture`]), which must agree with
    /// [`Self::apply_retry_for`]: a `ManualOnlyLatched { lapses: true }` bar and a
    /// `Scheduled` row are the same fact. The bar says this instead of "restart
    /// aterm to apply", which it said over the in-session handoff until
    /// 2026-08-30.
    pub(crate) fn apply_posture_for(&self, build: u64) -> crate::update_words::ApplyPosture {
        self.apply_posture_with(build, ApplyPostureEnv::observe(self))
    }

    /// The pure half of [`Self::apply_posture_for`]: the three answers about
    /// this process supplied, the lane's own state read from `self`. Order: an
    /// unavailable handoff first, whatever its reason (it changes WHETHER
    /// anything applies while a terminal is open, so it outranks everything that
    /// only changes WHEN), then policy (the config), then the screenshot seam (a
    /// development build's only), then this build's own stand-down latch; what is left is
    /// the automatic lane.
    pub(crate) fn apply_posture_with(
        &self,
        build: u64,
        env: ApplyPostureEnv,
    ) -> crate::update_words::ApplyPosture {
        use crate::app_config::AutoApplySetting;
        use crate::update_words::{ApplyPosture as P, AutoApplyVeto};
        if let Some(why) = env.handoff_unavailable {
            // The warning outranks policy, but its fallback clause depends on
            // it: a vetoed automatic lane never takes the cold apply at zero
            // live PTYs, so the bar must not promise that landing. Same
            // precedence as the standalone arms below.
            let veto = match env.auto_apply {
                AutoApplySetting::OffByConfig => Some(AutoApplyVeto::Config),
                AutoApplySetting::On if env.nudge_seam => Some(AutoApplyVeto::Env {
                    var: "ATERM_DEBUG_RELAUNCH_NUDGE",
                }),
                AutoApplySetting::On => None,
            };
            return P::HandoffDisabled { why, veto };
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
            return P::ManualOnlyLatched {
                lapses: manual.retry_at.is_some(),
            };
        }
        P::Automatic
    }

    /// One report from the updater's own check reached the event loop
    /// (`Wake::UpdateProgress`): the update's FLOW ROW moves on — with the
    /// apply posture computed for a `Staged` build, so the row says how it
    /// installs — which settles, re-grids and repaints. A check that DOWNLOADS
    /// is a check that works: a health warning that said checking (or, once
    /// the build is staged, downloading) was broken is over. The
    /// `Staged` report is emitted from the check thread BEFORE the `on_staged`
    /// callback posts `Wake::UpdateStaged`, so it lands here before the stage
    /// is reconciled and armed: only the POLICY posture is knowable now; the
    /// ARMED fact is re-stated onto the row a moment later by
    /// `arm_native_auto_apply`. A POSTPONED download posts no row: a live
    /// download row leaves quietly and the updater's sentence is a record.
    pub(crate) fn note_update_progress(&mut self, progress: &aterm_update::Progress) {
        use crate::messages_host::FlowPhase;
        // The updater's own sentence for how a download ended is the log's (and
        // the row's further detail): the row says it in the flow's words.
        match progress {
            aterm_update::Progress::Failed { detail } => {
                aterm_log::warn!("update download failed: {detail}");
            }
            aterm_update::Progress::Deferred { detail } => {
                aterm_log::info!("update download postponed: {detail}");
            }
            _ => {}
        }
        let posture = if let aterm_update::Progress::Staged { build, .. } = progress {
            Some(self.apply_posture_for(*build))
        } else {
            None
        };
        // A check that downloads is a check that works — and a download that
        // staged is a download that works; neither says an install does
        // (`App::heal_update_health` heals only the warning its proof answers).
        match progress {
            aterm_update::Progress::Downloading { .. }
            | aterm_update::Progress::Verifying { .. } => {
                self.heal_update_health(crate::messages_host::HealthProof::Checked);
            }
            aterm_update::Progress::Staged { .. } => {
                self.heal_update_health(crate::messages_host::HealthProof::Downloaded);
            }
            aterm_update::Progress::Deferred { .. } | aterm_update::Progress::Failed { .. } => {}
        }
        // The version a failed or postponed report does not carry: the live
        // download's.
        let downloading = self
            .live_update_flow()
            .filter(|flow| matches!(flow.phase, FlowPhase::Downloading | FlowPhase::Checking));
        let flow_version = downloading
            .map(|flow| flow.version.clone())
            .unwrap_or_default();
        let downloading = downloading.map(|flow| flow.id);
        let msg = crate::update_words::progress(progress, posture, &flow_version);
        if msg.hold == aterm_messages::Hold::LogOnly {
            // A RECORD (ruling 143) — a staged build that lands by itself later
            // or once the terminals close, a download postponed or failed: the
            // live download row, if one is up, is over first — `Ok` when the
            // download DELIVERED (a staged build: its Complete echo), `Warn` when
            // it failed (the Fault echo), and a postponement leaves with no
            // outcome to claim (a fade, never `⚠ failed`) — and the words are
            // recorded. A record never supersedes a live row (the engine's
            // rule, ruling 47), so the row is ended by id.
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
        // A staged DECISION is raised once per build and posture (ruling 119,
        // [`Self::ensure_staged_decision_in`]): a check that stages the same
        // build again under the same posture, after the person let the
        // question lapse, records it rather than asking again.
        let decision = match (progress, posture) {
            (aterm_update::Progress::Staged { build, .. }, Some(posture))
                if crate::update_words::staged_is_decision(Some(posture)) =>
            {
                Some((*build, posture))
            }
            _ => None,
        };
        if decision.is_some()
            && decision == self.staged_decision_raised
            && self.staged_update_row().is_none()
        {
            if let Some(id) = downloading {
                self.update_flow = None;
                self.resolve_message(id, aterm_messages::Outcome::Ok);
            }
            self.record_message(msg.hold(aterm_messages::Hold::LogOnly));
            return;
        }
        if decision.is_some() {
            self.staged_decision_raised = decision;
        }
        let (version, phase) = match progress {
            aterm_update::Progress::Downloading { version, .. } => {
                (version.clone(), FlowPhase::Downloading)
            }
            aterm_update::Progress::Verifying { version } => (version.clone(), FlowPhase::Checking),
            aterm_update::Progress::Staged { version, build } => (
                version.clone(),
                FlowPhase::Staged {
                    build: *build,
                    flow: crate::update_words::lane_is_working(posture),
                },
            ),
            // Both are records, answered above.
            aterm_update::Progress::Failed { .. } | aterm_update::Progress::Deferred { .. } => {
                return;
            }
        };
        // A report that moves only the METER — the download poller's next
        // byte count under the same words — restates the live row in place
        // (no log line; the meter is volatile), exactly as the toolchain
        // tailer's reads do. A report with new WORDS — the next phase — posts,
        // and the key supersedes the last phase's row so the log keeps every
        // one. Posted as-is the byte tick would be the center's Duplicate
        // (same key, same words) and the meter would never move.
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

    /// Re-state HOW the staged `build` installs on its live staged row, if the
    /// row is still up for it (`App::update_flow`, never its words), and repaint
    /// only when the words changed — the full words, so a change between the
    /// ready row and the flow row re-words, re-tones and re-lives the same row:
    /// a lane that installs by itself makes it LIVE again under its backstop
    /// (extended, never shortened), and one that waits for a press re-anchors
    /// the ready row's short hold (shortened, never extended). A no-op must not
    /// re-anchor a manual hold, so identical words restate nothing.
    ///
    /// EVERY site that changes the posture calls this — arming the lane and
    /// each of the physical stand-down paths that latch manual-only (the
    /// refused physical attempt, the policy fallback, the reaped abort, the
    /// returned-apply reconcile). Until 2026-08-30 only the first two did:
    /// an admission refusal is synchronous and lands inside the `Staged` row's
    /// hold, so the row kept promising the old words while the Version-menu
    /// row already said the attempt did not start — two surfaces, two answers.
    ///
    /// A posture whose words are a RECORD (ruling 143: a stand-down that
    /// retries later, the handoff off) folds the live staged row — withdrawn,
    /// no outcome to claim — and records the words. With no staged row up, a
    /// posture that has just become a DECISION (the lane stopped) raises the
    /// ready row once ([`Self::ensure_staged_decision`], ruling 119); anything
    /// else is a no-op.
    pub(crate) fn restate_staged_bar_posture(&mut self, build: u64) {
        use crate::messages_host::FlowPhase;
        let posture = self.apply_posture_for(build);
        let Some((id, version)) = self
            .live_update_flow()
            .filter(|flow| matches!(flow.phase, FlowPhase::Staged { build: b, .. } if b == build))
            .map(|flow| (flow.id, flow.version.clone()))
        else {
            if crate::update_words::staged_is_decision(Some(posture)) {
                self.ensure_staged_decision(build);
            }
            return;
        };
        let words = crate::update_words::staged(&version, build, Some(posture));
        if words.hold == aterm_messages::Hold::LogOnly {
            self.update_flow = None;
            self.withdraw_message(id);
            self.record_message(words);
            return;
        }
        if crate::update_words::staged_is_decision(Some(posture)) {
            self.staged_decision_raised = Some((build, posture));
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
        self.restate_update_flow(
            r,
            Some(FlowPhase::Staged {
                build,
                flow: crate::update_words::lane_is_working(Some(posture)),
            }),
        );
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
        .map(|trouble| trouble.with_retry_in(self.automatic_apply_retry_in(staged_build)))
    }

    /// How long until the automatic lane's next attempt at `staged_build`, when a
    /// latch carries the instant (2026-09-14, audit OBS-5). A live intent's own
    /// deadline is folded into the event loop and not surfaced here; the latch —
    /// the stand-down — is the one with a horizon a person plans around.
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
    /// the palette's Version row, a press on the staged status-bar row
    /// (the band row's `Install now` capsule), the off-macOS tab-strip ↻ — lands here. A strictly-newer STAGED build applies IMMEDIATELY via
    /// the process updater's one-shot apply authorization (which consumes the same
    /// `apply_staged_update_now` path after close preflight) — no intermediate overlay.
    /// With nothing actually staged
    /// (a stale nudge, the `ATERM_DEBUG_RELAUNCH_NUDGE` QA seam, a ledger cleared under
    /// us) it opens the Software Update route in the native Settings tab: honest
    /// details, never a dead click or a legacy modal — and nothing on this path
    /// ever restarts the app: a staged build is applied IN PLACE by the seamless
    /// overlap handoff, and the shells keep running.
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
        let target_build = self
            .native_updater_service
            .snapshot()
            .staged
            .as_ref()
            .map_or(0, |staged| staged.build);
        self.surface_update_apply_outcome_for_target(source, outcome, open_details, target_build);
    }

    /// A returned worker keeps its attempted build even if reconciliation has
    /// already imported a different stage. The current stage cannot identify
    /// which artifact failed.
    pub(crate) fn surface_update_apply_outcome_for_target(
        &mut self,
        source: &str,
        outcome: crate::native_app::UpdateOutcome,
        open_details: bool,
        target_build: u64,
    ) {
        self.record_apply_outcome_for_target_in_ledger(&outcome, target_build);
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
    /// status` reported `failing=0 failing_applies=0` and said the build was
    /// merely waiting to be applied,
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
    pub(crate) fn record_apply_outcome_in_ledger(
        &mut self,
        outcome: &crate::native_app::UpdateOutcome,
    ) {
        // Synchronous submission outcomes still describe this stage. A returned
        // worker uses the explicit-target entry point above: its reconciliation
        // may have retired this artifact and imported another one already.
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
        // ONE ROW PER UPDATE (2026-09-23; ruling 143): a retry the lane has
        // scheduled raises NOTHING on the glass — the flow row, where one is up,
        // already says the lane is on it, and the log keeps every attempt. Each
        // "Update postponed / waiting / delayed" row the owner read that day
        // described a retry he could do nothing about, and each covered the row
        // for 8 s and re-painted it. What the person CAN act on is a row: unsaved
        // editor work, and a lane that stopped (`Install now`, the Version
        // menu's press on the glass).
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
            crate::native_app::UpdateOutcome::InstalledNeedsRelaunch { build, message } => {
                let retry_scheduled = self.automatic_apply_retry_scheduled(build);
                aterm_log::warn!(
                    "update apply ({source}): build {build} is installed on disk; activation \
                     pending (automatic retry scheduled={retry_scheduled}): {message}"
                );
                if open_details {
                    let _ = self
                        .open_settings_tab(crate::native_settings::SettingsRoute::SoftwareUpdate);
                } else if retry_scheduled {
                    self.restate_staged_bar_posture(build);
                } else {
                    // The download's ready row is obsolete now that its bytes
                    // are installed; the switch is the person's to request, and
                    // the decision says so with its press.
                    self.retire_staged_update_row(true);
                    self.note_update_outcome(crate::update_words::needs_install(
                        UPDATE_INSTALLED_TITLE,
                        UPDATE_INSTALLED_DETAIL,
                        aterm_messages::Severity::Info,
                        build,
                    ));
                }
            }
            crate::native_app::UpdateOutcome::Deferred { reason } => {
                aterm_log::info!("update apply ({source}) deferred: {reason}");
                // THE SCREEN FROZE AND THEN CAME BACK. On the automatic lane a
                // deferral lands AFTER the readers parked (the user touched the
                // keyboard, and the lane stood down for them): the flow row the
                // attempt rewrote comes back with its words (`retire_update_installing`)
                // and says the lane is still on it. Only a lane that will NOT
                // come back by itself, with no ready row up to say how the build
                // installs, owes the person a row.
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
            crate::native_app::UpdateOutcome::Failed { message } => {
                aterm_log::warn!("update apply ({source}) failed safely: {message}");
                if open_details {
                    self.retire_charging_surge();
                    let _ = self
                        .open_settings_tab(crate::native_settings::SettingsRoute::SoftwareUpdate);
                } else if let Some((staged_build, version)) = staged {
                    // WHICH ANSWER IS TRUE IS A QUESTION ABOUT SCHEDULING STATE
                    // (`automatic_apply_retry_scheduled` asks every carrier of it:
                    // a live intent, or a manual-only latch with a lapse deadline,
                    // each for THIS artifact, with automatic apply enabled). A
                    // physical failure inside its budget schedules another attempt,
                    // so the flow row is re-stated to the stand-down's words and
                    // nothing else is said — unless a PERSON asked for this one: a
                    // person's failure charges the lane nothing, so the flow row
                    // would not change, and whoever just asked is exactly who must
                    // be told it did not happen (once, and that the lane has it). A
                    // lane that has stopped names the one control that moves it.
                    if retry_scheduled {
                        self.retire_charging_surge();
                        self.restate_staged_bar_posture(staged_build);
                        if !source_is_automatic(source) {
                            // The person's own press: its switch row ends in the
                            // Fault echo, and the lane has it — a self-retry is a
                            // RECORD (ruling 143).
                            self.note_update_outcome(crate::update_words::outcome(
                                '\u{21bb}',
                                &format!("Couldn't install aterm v{version}"),
                                "will try again by itself",
                                aterm_messages::Severity::Info,
                            ));
                        }
                    } else {
                        self.note_update_outcome(crate::update_words::needs_install(
                            &format!("Couldn't install aterm v{version}"),
                            crate::update_words::INSTALL_FROM_MENU,
                            aterm_messages::Severity::Warn,
                            staged_build,
                        ));
                    }
                } else {
                    // A retired/consumed artifact is not "still ready". The row
                    // says only that it did not finish: the details page is the
                    // Warn row's `Software Update` capsule and its `Details ›`
                    // (R37: the "click for details" clause became those capsules),
                    // so the row carries no excerpt.
                    self.note_update_outcome(crate::update_words::failed(
                        UPDATE_DIDNT_FINISH,
                        "",
                        false,
                    ));
                    // …and the ready row does not stand beside it: the
                    // artifact this row was offering is the one that is gone,
                    // so it is WITHDRAWN — resolved `Ok`, the automatic lane's
                    // busy `Installing aterm vX` would echo `✓ Installed …`
                    // beside this failure (ruling 159).
                    self.retire_staged_update_row(false);
                }
            }
        }
    }

    /// A close preflight refused the AUTOMATIC install of `build`: re-state the
    /// posture on its row ([`Self::restate_staged_bar_posture`]) and, while the
    /// refusal is work a person has to save ([`App::update_blocker_for_person`]),
    /// have the flow row say so instead of "installs within a minute"
    /// ([`Self::restate_update_flow_holds`], `Holds::Editor`). Words only — a
    /// restatement, never a row.
    pub(crate) fn restate_blocked_update_bar(&mut self, build: u64, reasons: &[String]) {
        self.restate_staged_bar_posture(build);
        if Self::update_blocker_for_person(reasons).is_some() {
            let _ = self.restate_update_flow_holds(build, crate::update_words::Holds::Editor);
        }
    }

    /// A close preflight refused the install: name the one blocker a PERSON can
    /// clear ([`App::update_blocker_for_person`] — unsaved editor or Settings
    /// work) — on the flow row when it is up (it already says so,
    /// [`Self::restate_blocked_update_bar`]), as an outcome row otherwise — and say
    /// nothing else while the lane will try again by itself; a lane that has
    /// stopped points at the Version menu.
    pub(crate) fn note_update_blockers(&mut self, reasons: &[String], retry_scheduled: bool) {
        if let Some(blocker) = Self::update_blocker_for_person(reasons) {
            let staged = self
                .native_updater_service
                .snapshot()
                .staged
                .as_ref()
                .map(|stage| stage.build);
            if staged
                .and_then(|build| self.update_flow_row_for(build))
                .is_none()
            {
                // The blocker IS what the person does: painted (ruling 77).
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

    /// A lane that STOPPED, titled `title` (ruling 143): a decision row whose
    /// `Install now` is the Version menu's press for the staged build — or,
    /// with no build staged to press for, the same words on record.
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
                crate::update_words::INSTALL_FROM_MENU,
                aterm_messages::Severity::Warn,
                build,
            ),
            None => crate::update_words::outcome(
                '\u{26a0}',
                title,
                crate::update_words::INSTALL_FROM_MENU,
                aterm_messages::Severity::Warn,
            ),
        }
    }

    /// One apply-lane OUTCOME (ruling 143): a stopped lane, an install that
    /// needs a press, a person's failed attempt — a row of its own
    /// (`update.outcome`) beside the flow row when the person acts on it, a
    /// RECORD when the lane answers it by itself (`msg`'s hold says which,
    /// `update_words`). A charging surge is retired with it: the attempt it
    /// explained is over.
    pub(crate) fn note_update_outcome(&mut self, msg: aterm_messages::Message) {
        // Over an "installing…" row the attempt is over: the words it replaced
        // come back first (or the row it added faults out), so the outcome
        // stands beside the flow row.
        self.retire_update_installing();
        if msg.hold == aterm_messages::Hold::LogOnly {
            self.record_message(msg);
            return;
        }
        // ONE DECISION PER STOPPED LANE (rulings 119 and 143; review
        // 2026-09-24). An outcome that carries `Install now` IS the lane's
        // decision: a staged row still up — the ready row a stand-down's
        // posture restatement just turned the flow row into — would be a
        // second row with the same press for the same fact. It leaves first,
        // withdrawn (no outcome to claim: the attempt it offered failed), so
        // the outcome's words — why it did not install — are the one row.
        if msg
            .actions
            .iter()
            .any(|a| matches!(a, aterm_messages::Intent::ApplyUpdate { .. }))
            && let Some(id) = self.staged_update_row()
        {
            self.update_flow = None;
            self.withdraw_message(id);
        }
        // A HELD outcome this one replaces is resolved first, so `appstatus`
        // keeps every outcome of one update as a finished activity (a
        // superseded row records nothing).
        self.resolve_held_row_under(crate::update_words::KEY_OUTCOME);
        self.post_message(msg);
    }

    /// The update lane's press rule for its `Install now` capsule: a STAGED row
    /// whose build is really ready applies it in place (the retired floating
    /// card's one-click gesture, moved to the row) — but never where the
    /// seamless handoff is off: the row carries no capsule there, and a press
    /// that could only record a refusal opens the details page instead, like
    /// every other row. Pure, so the two press paths (mouse, screen reader)
    /// cannot drift (`App::perform_intent`, the `ApplyUpdate` arm).
    pub(crate) fn update_bar_press_applies(
        bar_is_staged: bool,
        staged_ready: bool,
        handoff_available: bool,
    ) -> bool {
        bar_is_staged && staged_ready && handoff_available
    }

    /// The surge's motion amplitude for `phase`, or `None` when this phase must
    /// not be spawned at all.
    ///
    /// SERIOUS MODE SPLITS THE TWO PHASES, because they are not the same kind of
    /// thing (2026-09-09). `LANDING` is a celebration: serious mode removes it,
    /// like every other decorative effect. `CHARGING` is not decorative — it is
    /// the ONLY thing on screen that says why the terminal stopped echoing, and
    /// on the automatic lane with no update row up (its ready row folded, or it
    /// never had one) it is the only explanation the user gets at all. Removing
    /// it hands someone in serious mode a terminal that freezes for seconds with
    /// no reason given. `motion.rs`'s own charter draws this line: serious mode
    /// "removes decorative output, but must not demote functional motion … and
    /// it must not affect cursor blink, visual bell, or window attention". A
    /// freeze explanation belongs with the visual bell, not with the confetti.
    ///
    /// So serious mode keeps the charging rim and takes its MOVEMENT instead —
    /// amplitude `0`, the same still rim Reduce Motion resolves (information
    /// kept, movement removed). No pulse, no hue travel, no thickening: a
    /// steady inset rim that says "working" and nothing more.
    fn upgrade_surge_motion(&self, phase: crate::level_up::Phase) -> Option<f32> {
        let serious = !self
            .serious_mode_policy()
            .allows(crate::motion::SeriousEffect::LevelUp);
        match (phase, serious) {
            (crate::level_up::Phase::Landing, true) => None,
            (_, true) => Some(0.0),
            (_, false) => Some(
                self.motion_policy(true)
                    .amplitude(crate::motion::MotionEffect::UpgradeSurge),
            ),
        }
    }

    /// The UPGRADE SURGE (`crate::level_up`): spawn `phase` for `build` under
    /// the amplitude [`Self::upgrade_surge_motion`] resolves — which is where
    /// serious mode and Reduce Motion are applied.
    pub(crate) fn spawn_upgrade_surge(&mut self, phase: crate::level_up::Phase, build: u64) {
        let Some(motion) = self.upgrade_surge_motion(phase) else {
            return;
        };
        let now = std::time::Instant::now();
        self.level_up = Some(match phase {
            crate::level_up::Phase::Charging => {
                crate::level_up::LevelUp::charging(build, now, motion)
            }
            crate::level_up::Phase::Landing => {
                crate::level_up::LevelUp::landing(build, now, motion)
            }
        });
        self.request_redraw_all_windows();
    }

    /// The SUCCESSOR's half of a charging rim: the outgoing process has been
    /// charging its rim from the park, so this process's starts already charged
    /// — no ramp-in dip, no rebuild — and the swap reads as one continuous rim.
    /// Gated exactly like [`Self::spawn_upgrade_surge`].
    pub(crate) fn spawn_upgrade_surge_continued(&mut self, build: u64) {
        let Some(motion) = self.upgrade_surge_motion(crate::level_up::Phase::Charging) else {
            return;
        };
        self.level_up = Some(crate::level_up::LevelUp::charging_continued(
            build,
            std::time::Instant::now(),
            motion,
        ));
        self.request_redraw_all_windows();
    }

    /// Drop a CHARGING surge (an attempt ended without the successor taking
    /// over); a landing is left to finish on its own.
    pub(crate) fn retire_charging_surge(&mut self) {
        if self
            .level_up
            .as_ref()
            .is_some_and(|l| l.phase() == crate::level_up::Phase::Charging)
        {
            self.level_up = None;
            self.request_redraw_all_windows();
        }
    }

    /// APPLY BEGINS (before the carry is built, before the readers park): the
    /// update bar says "installing" and the border surge starts charging. With
    /// the row already up its WORDS change — a repaint, not a re-grid. With no
    /// row up, an EXPLICIT apply (`explicit`: the Version menu, Software Update,
    /// a clean quit) ADDS the row and commits the re-grid at once, so the carry
    /// built next reads the rows the successor must reproduce; the automatic
    /// lane adds none — its own re-check reads the re-grid's SIGWINCH as
    /// activity and stands down — and there the surge alone explains the frozen
    /// frame. What the row said before is kept so a refusal can put it back
    /// (`retire_update_installing`).
    pub(crate) fn begin_update_installing(&mut self, build: u64, explicit: bool) {
        use crate::messages_host::FlowPhase;
        let version = self
            .native_updater_service
            .snapshot()
            .staged
            .as_ref()
            .map(|staged| staged.version.clone())
            .unwrap_or_default();
        let installing = crate::update_words::installing(&version);
        self.update_row_before_install = None;
        let live = self
            .live_update_flow()
            .cloned()
            .and_then(|flow| self.messages.live(flow.id).map(|l| (flow, l.msg.clone())));
        if let Some((flow, previous)) = live {
            // The flow row is re-worded in place — a repaint, not a re-grid; no
            // log line.
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
            // Posted through the seam that commits the re-grid at once.
            self.post_update_row(installing, &version, FlowPhase::Installing);
        }
        self.spawn_upgrade_surge(crate::level_up::Phase::Charging, build);
        self.request_redraw_all_windows();
    }

    /// The handoff did not take over — a synchronous refusal before the park, or
    /// an asynchronous one after it (a failed proof, a revoked lane, a dead
    /// successor): the row goes back to what it was — its words, its animation
    /// and its phase — (or to no row) and the charging surge ends. Idempotent,
    /// and a no-op for a row an outcome has already been posted over. The row is
    /// identified by its STATE (`FlowPhase::Installing`), never its words.
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
                // The explicit lane added it; the refusal takes it away — a
                // refusal is not a delivery, so it resolves `Warn` and its
                // indicator ends in the Fault echo (design §10.2 #7).
                None => {
                    self.messages
                        .resolve(id, aterm_messages::Outcome::Warn, now);
                    self.update_flow = None;
                }
            }
        }
        self.retire_charging_surge();
        // WORDS ONLY — no row sync here. A refusal is followed, in the same
        // wake, by the outcome that explains it (`surface_update_apply_outcome`),
        // and on the explicit lane that outcome takes the very row this attempt
        // added: a sync here would re-grid every window twice (1→0→1). The one
        // trailing sync — `Wake::UpdateHandoffFinished`'s, or the next park's
        // row check in `about_to_wait` — commits whatever the center finally wants.
        self.request_redraw_all_windows();
    }

    /// Re-sync the macOS VERSION menu (the rightmost menu-bar title) to the live update
    /// state: `v<cur> ⬆️` + the one-click "Install aterm v<staged> now" first item
    /// while a strictly-newer build is staged; `v<cur> ⬆️` + the "Updated to aterm v<cur> just
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
            // Version menu that says "apply now" while the engine has already tried
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
pub(crate) mod tests {
    use super::{
        ApplyLedgerVerdict, ApplyPostureEnv, UPDATE_DIDNT_INSTALL, UPDATE_WAITS_FOR_YOU,
        apply_ledger_verdict, debug_seamless_reexec_armed,
    };
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
            aterm_types::control_socket::env_flag_engaged(
                std::env::var_os("ATERM_DEBUG_SEAMLESS_REEXEC")
                    .map(|v| v.to_string_lossy().into_owned())
                    .as_deref(),
            ),
            "the seam is armed exactly when its variable is ENGAGED (non-empty and not \
             \"0\") — the one flag rule every ATERM_NO_* / QA-seam reader shares, so an \
             inherited empty variable can never arm it"
        );
        assert_eq!(
            armed,
            debug_seamless_reexec_armed(),
            "and the cached answer is stable across callers"
        );
    }

    /// THE PRESS RULE for the update row (2026-09-07): only a Staged row whose
    /// build is really ready applies on a press; every other row opens the
    /// details page. Pure, and shared by the mouse and the screen reader.
    #[test]
    fn the_update_row_applies_on_a_press_only_when_staged_and_ready() {
        assert!(App::update_bar_press_applies(true, true, true));
        assert!(!App::update_bar_press_applies(true, false, true));
        assert!(!App::update_bar_press_applies(false, true, true));
        assert!(!App::update_bar_press_applies(false, false, true));
        assert!(
            !App::update_bar_press_applies(true, true, false),
            "where the handoff is off the title carries no affordance and a press \
             opens the details rather than recording a refusal"
        );
        // On glass: a staged ROW with nothing really ready (the test ledger has
        // no strictly-newer build; a headless process has no handoff either)
        // opens the details route — never a modal, never a ledger entry.
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
            crate::messages_host::FlowPhase::Staged {
                build: 1,
                flow: false,
            },
        );
        assert_eq!(app.staged_update_row(), Some(id));
        app.perform_intent(
            WindowId(0),
            id,
            aterm_messages::Intent::ApplyUpdate { build: 1 },
        );
        let (_, view) = app
            .active_native_view(WindowId(0))
            .expect("native Settings tab");
        assert!(matches!(
            app.native_runtime.view_state(view),
            Some(AppViewState::Settings(state))
                if state.route == crate::native_settings::SettingsRoute::SoftwareUpdate
        ));
    }

    /// THE SURGE AND THE ROW AT THE APP'S SEAMS (2026-09-07): apply-begins
    /// charges the rim and rewrites the row; a refusal restores both, and is
    /// idempotent; an explicit apply with no row up adds one and a refusal
    /// removes it; the automatic lane never adds one; a landing outlives the
    /// outcome posted with it while a charging surge does not.
    #[test]
    fn apply_begins_charges_the_rim_and_a_refusal_restores_the_row() {
        use crate::level_up::Phase;
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = App::headless_for_test();
        let phase = |app: &App| app.level_up.as_ref().map(crate::level_up::LevelUp::phase);
        let staged = aterm_update::Progress::Staged {
            version: "9.9.9".into(),
            build: 41,
        };
        app.post_update_row(
            crate::update_words::progress(&staged, None, ""),
            "9.9.9",
            crate::messages_host::FlowPhase::Staged {
                build: 41,
                flow: false,
            },
        );
        assert!(app.update_row_text().unwrap().contains("is ready"));

        app.begin_update_installing(41, false);
        assert_eq!(phase(&app), Some(Phase::Charging));
        // (No staged VERSION in the test ledger: the title says "update".)
        assert_eq!(
            app.update_row_text().unwrap(),
            "Installing update \u{2014} installing\u{2026}"
        );
        assert_eq!(
            app.messages.live_rows().count(),
            1,
            "the row was rewritten, not added"
        );
        app.retire_update_installing();
        assert_eq!(phase(&app), None);
        assert!(
            app.update_row_text().unwrap().contains("is ready"),
            "the words go back"
        );
        app.retire_update_installing();
        assert!(
            app.update_row_text().unwrap().contains("is ready"),
            "idempotent"
        );

        // No row up: the automatic lane adds none; an explicit apply adds one,
        // and a refusal takes it away again.
        app.clear_messages_for_test();
        assert_eq!(app.message_band_rows, 0);
        app.begin_update_installing(41, false);
        assert_eq!(
            app.messages.live_rows().count(),
            0,
            "the automatic lane never adds a row"
        );
        assert_eq!(app.message_band_rows, 0);
        assert_eq!(
            phase(&app),
            Some(Phase::Charging),
            "…but the surge still charges"
        );
        app.retire_update_installing();
        assert_eq!(phase(&app), None);
        app.begin_update_installing(41, true);
        assert_eq!(
            app.messages.live_rows().count(),
            1,
            "an explicit apply adds the row"
        );
        assert_eq!(
            app.message_band_rows, 1,
            "…and commits it at once, before the carry reads the window"
        );
        assert!(
            app.update_row_text()
                .unwrap()
                .contains("installing\u{2026}")
        );
        app.retire_update_installing();
        assert_eq!(app.messages.live_rows().count(), 0);
        assert_eq!(
            app.message_band_rows, 1,
            "a refusal retires the words without a re-grid of its own…"
        );
        // …the trailing sync (the completion wake's, or the next park's) commits
        // the count once the outcome has had its say — the D1 hysteresis
        // shrinks only after its quiet, measured from the first sync that saw
        // the smaller want.
        let now = std::time::Instant::now();
        let _ = app.settle_messages(now);
        assert_eq!(
            app.message_band_rows, 1,
            "grow at once, shrink after the quiet"
        );
        assert!(app.settle_messages(now + aterm_messages::SHRINK_QUIET));
        assert_eq!(app.message_band_rows, 0);

        // A landing outlives the outcome posted with it; a charging surge ends
        // with it, and the outcome's words stand beside the row's.
        app.spawn_upgrade_surge(Phase::Landing, 41);
        app.note_update_outcome(crate::update_words::failed(
            super::UPDATE_WAITS_FOR_YOU,
            App::UNSAVED_NATIVE_WORK_BLOCKS_APPLY,
            true,
        ));
        assert_eq!(phase(&app), Some(Phase::Landing));
        app.level_up = None;
        app.clear_messages_for_test();
        app.post_update_row(
            crate::update_words::progress(&staged, None, ""),
            "9.9.9",
            crate::messages_host::FlowPhase::Staged {
                build: 41,
                flow: false,
            },
        );
        app.begin_update_installing(41, false);
        app.note_update_outcome(crate::update_words::failed(
            super::UPDATE_WAITS_FOR_YOU,
            App::UNSAVED_NATIVE_WORK_BLOCKS_APPLY,
            true,
        ));
        assert_eq!(phase(&app), None);
        assert!(
            app.update_row_text()
                .unwrap()
                .contains(App::UNSAVED_NATIVE_WORK_BLOCKS_APPLY)
        );
        app.retire_update_installing();
        assert!(
            app.update_row_text()
                .unwrap()
                .contains(App::UNSAVED_NATIVE_WORK_BLOCKS_APPLY),
            "a row an outcome moved on is not retired"
        );
        // The outcome stands beside the Staged row the attempt had rewritten:
        // the ready row is back, in place, and outlives the outcome's fold.
        assert!(
            app.update_row_detail() == Some(crate::update_words::staged_detail_unknown()),
            "the refusal put the ready row's words back: {:?}",
            app.update_row_detail()
        );
        assert!(app.settle_messages(std::time::Instant::now() + aterm_messages::HOLD_WARN));
        assert!(
            app.update_row_text().unwrap().contains("is ready"),
            "the staged row stands once the outcome has had its say"
        );
        assert_eq!(app.messages.live_rows().count(), 1);
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
            linux_host: false,
            linux: None,
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
        });
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn returned_failure_ledger_conforms_to_attempt_target_after_stage_replacement() {
        use crate::native_updater_service::{
            ApplyDecision, ApplyMode, ApplyPreflightStart, ClosePreflight,
            ReturnedApplyDisposition, ReturnedApplyFacts,
        };
        let _ledger = super::hold_update_ledger_for_test();
        let model = aterm_spec::derive::native_update_failure_target_model();
        for replaced in [false, true] {
            let mut app = App::headless_for_test();
            let running = app.native_updater_service.snapshot().current_build;
            let original = running + 65_101 + u64::from(replaced) * 2;
            let replacement = original + 1;
            stage_build_with_ledger(&mut app, original);
            let ApplyPreflightStart::Inspect(preflight) = app
                .native_updater_service
                .begin_apply_preflight(ApplyMode::Immediate)
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
                // Historical path derived the target from the current stage.
                // Drive it against the real ledger to prove the distinction is
                // observable, then require the model to reject that result.
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
            // A retry the lane has scheduled posts NO row (2026-09-23): the flow
            // row, where one is up, already says the lane is on it.
            let scheduled = automatic && armed;
            let installed = app.update_row_text();
            if scheduled {
                assert_eq!(installed, None, "no row for a retry that happens by itself");
            } else {
                assert_eq!(
                    installed.as_deref(),
                    Some("Update installed \u{2014} finish it from the Version menu")
                );
            }

            app.react_to_update_apply_outcome(
                "manual",
                UpdateOutcome::Deferred {
                    reason: "typing gap".to_string(),
                },
                false,
            );
            let deferred = app.update_row_text();
            if scheduled {
                assert_eq!(deferred, None, "no row for a retry that happens by itself");
            } else {
                assert_eq!(
                    deferred,
                    Some(format!(
                        "{UPDATE_DIDNT_INSTALL} \u{2014} {}",
                        crate::update_words::INSTALL_FROM_MENU
                    ))
                );
            }
        }
    }

    /// THE STATUS BAR'S POSTURE, IN THE ORDER THE FACTS OUTRANK EACH OTHER: the
    /// disabled handoff (nothing applies while a terminal is open), then policy
    /// — config, then the
    /// environment — then the screenshot seam, then THIS build's own stand-down
    /// latch (a lapsing latch is the same fact as a `Scheduled` retry row; a
    /// sticky one waits for a click); what is left is the automatic lane. The
    /// three answers about the process go in as parameters: the tests in this
    /// crate assert those variables' ABSENCE, and setting process env would
    /// poison every one of them. The LIVE reader is pinned on what a headless
    /// App really is — a process with no seamless lane — because it reads the
    /// same predicate the apply gate reads.
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
                && std::env::var_os("ATERM_CONTROL_SOCK").is_none(),
            "PRECONDITION: no update veto may be set in the test environment"
        );
        let build = stage_one_build(&mut app);
        let latch = |build: u64, retry_at: Option<std::time::Instant>| crate::AutoApplyManualOnly {
            build,
            dmg_sha256: [0xab; 32],
            retry_at,
        };
        let later = Some(std::time::Instant::now() + std::time::Duration::from_secs(600));
        let clean = ApplyPostureEnv {
            handoff_unavailable: None,
            auto_apply: AutoApplySetting::On,
            nudge_seam: false,
        };
        // THE LIVE READER, on a headless App: no window and no event loop, so no
        // seamless lane — and the posture says so, because it reads the ONE
        // predicate the apply gate reads. (Until 2026-08-30 it folded only the
        // opt-out and answered `Automatic` here, over a lane the gate refused.)
        assert_eq!(
            app.seamless_handoff_unavailable(),
            Some(HandoffUnavailable::Headless)
        );
        assert_eq!(
            ApplyPostureEnv::observe(&app).handoff_unavailable,
            app.seamless_handoff_unavailable()
        );
        assert_eq!(
            app.apply_posture_for(build),
            P::HandoffDisabled {
                why: HandoffUnavailable::Headless,
                veto: None
            },
            "a headless App has no seamless lane, and the bar says so"
        );
        assert_eq!(
            app.apply_posture_with(build, clean),
            P::Automatic,
            "the default policy, with the lane available"
        );

        // THE LANE'S OWN STATE: a lapsing latch, a sticky latch, another build's.
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

        // POLICY OUTRANKS THE LATCH: config first…
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
        // …the screenshot seam (a development build's) names its variable…
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
        // …and an unavailable handoff outranks all of it, whatever its reason:
        // nothing applies while a terminal is open, so neither the automatic
        // promise nor the manual affordance would be true — but the warning
        // CARRIES the veto (config here: it outranks the seam, like the arms
        // above), so the bar's fallback clause cannot promise the
        // closed-terminals landing a vetoed lane never attempts.
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
                    veto: Some(crate::update_words::AutoApplyVeto::Config)
                }
            );
        }
        // The screenshot seam is a veto of its own when policy is otherwise On…
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
                })
            }
        );
        // …and an armed lane carries none.
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
                veto: None
            }
        );

        // THE LIVE READER folds the real config through the same seam…
        app.auto_apply_manual_only = None;
        app.config.update = Some(crate::app_config::UpdateConfig {
            auto_apply: Some(false),
            ..app.config.update.clone().unwrap_or_default()
        });
        assert_eq!(
            crate::app_config::update_auto_apply_setting(&app.config),
            AutoApplySetting::OffByConfig
        );
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
        // …and, headless, still answers with the lane that is not there — now
        // carrying the config veto the live reader observed.
        assert_eq!(
            app.apply_posture_for(build),
            P::HandoffDisabled {
                why: HandoffUnavailable::Headless,
                veto: Some(crate::update_words::AutoApplyVeto::Config)
            }
        );
    }

    /// THE BAR CARRIES THE POSTURE THE APP COMPUTED. `Wake::UpdateProgress` lands
    /// in `note_update_progress`; a `Staged` report paints `apply_posture_for`,
    /// which for a headless App — no window, no event loop, so no seamless lane
    /// — is the handoff-off warning: the same fact the apply gate acts on
    /// (`a_stand_down_restates_the_staged_bar_while_it_is_still_up` drives that
    /// gate). Never "applies in place within ~2 min" over a lane that is not
    /// there, never "apply it from the Version menu" for a click the gate
    /// refuses, never "restart aterm to apply". With the lane AVAILABLE (the pure
    /// seam) the automatic promise is painted, and a later stand-down for that
    /// build is re-stated onto the same bar.
    #[test]
    fn a_staged_report_paints_the_bar_with_the_posture_the_app_computed() {
        use crate::app_config::AutoApplySetting;
        use crate::app_update_handoff::HandoffUnavailable;
        use crate::update_words::{ApplyPosture, staged_detail};
        let mut app = App::headless_for_test();
        assert!(
            crate::app_config::update_auto_apply(&app.config)
                && !App::relaunch_nudge_seam_suppresses_auto_apply()
                && std::env::var_os("ATERM_CONTROL_SOCK").is_none(),
            "PRECONDITION: no update veto may be set in the test environment"
        );
        let build = stage_one_build(&mut app);
        let staged = aterm_update::Progress::Staged {
            version: "9.9.9".to_string(),
            build,
        };
        let bar_detail = |app: &App| app.update_row_detail().expect("the update row is up");
        // Ruling 143: a staged build nothing can press, that the lane does not
        // land within a minute, is a RECORD — the log carries the posture's
        // words, the glass never shows them.
        let staged_record = |app: &App| {
            app.messages
                .log()
                .records()
                .rev()
                .find(|r| r.key.as_deref() == Some(crate::update_words::KEY_PROGRESS))
                .filter(|r| r.retired() == Some(&aterm_messages::Retired::Recorded))
                .map(|r| (r.title.clone(), r.detail.join("; ")))
                .expect("the staged report is recorded")
        };
        app.note_update_progress(&staged);
        assert!(
            app.messages
                .live_by_key(crate::update_words::KEY_PROGRESS)
                .is_none(),
            "a staged build nothing can press is not a band row"
        );
        let (title, detail) = staged_record(&app);
        assert_eq!(title, "aterm v9.9.9 is ready");
        let headless = ApplyPosture::HandoffDisabled {
            why: HandoffUnavailable::Headless,
            veto: None,
        };
        assert_eq!(detail, staged_detail(headless));
        assert!(
            detail.contains("--headless")
                && detail.contains("installs once every terminal is closed")
                && !detail.contains("minute")
                && !detail.contains("Version menu")
                && !detail.to_lowercase().contains("restart"),
            "{detail}"
        );
        assert_eq!(app.message_band_rows, 0, "…and no row is reserved");

        // A stand-down latch for this build changes nothing here: the lane that
        // cannot run outranks the latch, and nothing is re-stated.
        let latch = |retry_at| crate::AutoApplyManualOnly {
            build,
            dmg_sha256: [0xab; 32],
            retry_at,
        };
        app.auto_apply_manual_only = Some(latch(None));
        assert_eq!(app.apply_posture_for(build), headless);
        let before = app.messages.revision();
        app.restate_staged_bar_posture(build);
        assert_eq!(app.messages.revision(), before, "the words did not change");

        // With the lane AVAILABLE — the pure seam, since a headless test process
        // cannot build an event loop — the automatic promise, then the stand-down
        // re-stated onto the same bar.
        let available = ApplyPostureEnv {
            handoff_unavailable: None,
            auto_apply: AutoApplySetting::On,
            nudge_seam: false,
        };
        app.auto_apply_manual_only = None;
        assert_eq!(
            app.apply_posture_with(build, available),
            ApplyPosture::Automatic
        );
        app.post_update_row(
            crate::update_words::progress(&staged, Some(ApplyPosture::Automatic), ""),
            "9.9.9",
            crate::messages_host::FlowPhase::Staged { build, flow: true },
        );
        let detail = bar_detail(&app);
        assert_eq!(detail, "installs within a minute \u{2014} keep working");
        assert_eq!(
            app.update_row_text().unwrap(),
            "Installing aterm v9.9.9 \u{2014} installs within a minute \u{2014} keep working"
        );
        app.auto_apply_manual_only = Some(latch(None));
        let posture = app.apply_posture_with(build, available);
        assert_eq!(posture, ApplyPosture::ManualOnlyLatched { lapses: false });
        assert!(app.restate_update_flow(
            crate::update_words::restate_apply_posture("9.9.9", build, posture),
            None,
        ));
        let detail = bar_detail(&app);
        assert!(
            detail.contains("automatic install") && !detail.to_lowercase().contains("restart"),
            "{detail}"
        );
        let row = app
            .messages
            .live_by_key(crate::update_words::KEY_PROGRESS)
            .expect("the staged row");
        assert_eq!(row.msg.title, "aterm v9.9.9 is ready");
        assert_eq!(
            row.msg.actions,
            vec![aterm_messages::Intent::ApplyUpdate { build }],
            "a lane that stopped offers the press"
        );
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
    /// The ledger guard protects the single standing target slot across writes and
    /// reads. Unique build numbers alone cannot prevent a sibling failure writer
    /// from replacing that slot between those operations.
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
            "\u{2191} Install aterm v9.9.9 now"
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
            "\u{2191} Install aterm v9.9.9 \u{2014} tried once, didn\u{2019}t start; \
             will try again"
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
            " Details in Settings \u{25b8} Messages."
        } else {
            ""
        };
        assert_eq!(
            projection.detail.as_deref(),
            Some(
                format!(
                    "Version 1.0.{build} \u{b7} aterm tried to update once and the new \
                     version did not finish starting. It will try again by itself.{log_pointer}"
                )
                .as_str()
            )
        );
        assert_eq!(
            crate::native_settings::compact_update_detail(&projection),
            "Tried once, didn\u{2019}t start \u{b7} will try again."
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
            "\u{2191} Install aterm v9.9.9 \u{2014} tried once, didn\u{2019}t start; \
             try again now"
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
            "\u{2191} Install aterm v9.9.9 \u{2014} tried once, didn\u{2019}t take over; \
             try again now",
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
            "\u{2191} Install aterm v9.9.9 now"
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
            "\u{2191} Install aterm v9.9.9 \u{2014} tried once, too slow to start; \
             will try again",
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
                    installable: true,
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
            // The former build-only predicate says Scheduled for both cases;
            // the mismatched digest is the required negative control.
            assert!(
                app.auto_apply_manual_only
                    .is_some_and(|latch| { latch.build == build && latch.retry_at.is_some() })
            );
        }
        app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
            build,
            dmg_sha256: [0xab; 32],
            retry_at: Some(later),
        });
        assert_eq!(
            app.native_updater_service
                .reconcile_durable_stage(true, None, None, None, None),
            crate::native_updater_service::DurableStageDisposition::Retired
        );
        assert!(!app.automatic_apply_retry_scheduled(build));
    }

    /// A RETRY THE LANE SCHEDULED POSTS NO ROW; A LANE THAT STOPPED SAYS SO ONCE.
    ///
    /// "Update paused — manual retry" named a mechanism, not an action, and it was
    /// flatly wrong half the time: a physical handoff failure inside its budget
    /// schedules another automatic attempt. Which answer is true is a question
    /// about SCHEDULING STATE, so every carrier of it is consulted — a live
    /// intent, or a latch with a deadline — and each must name THIS staged
    /// artifact. Since 2026-09-23 the scheduled answer is SILENCE: the flow row
    /// says the lane is on it, and the owner read every "delayed / waiting /
    /// postponed" row that day as noise about a retry he could do nothing about.
    /// Only a PERSON whose own attempt failed is told, because nothing else would
    /// tell them.
    ///
    /// The user-facing strings are the assertion, because they are the contract.
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
        let version = app
            .native_updater_service
            .snapshot()
            .staged
            .as_ref()
            .map(|stage| stage.version.clone())
            .expect("a stage");
        let failed = || UpdateOutcome::Failed {
            message: "overlap handoff failed safely: handoff proof ended TimedOut".to_string(),
        };
        // THE ROW, NOT A PILL (2026-09-07): the outcome is the update bar's
        // words — "<title> — <detail>" — and silence is an empty lane.
        let row = |app: &App| app.update_row_text();
        let stopped = Some(format!(
            "Couldn't install aterm v{version} \u{2014} {}",
            crate::update_words::INSTALL_FROM_MENU
        ));

        // NOTHING SCHEDULED: no intent, no latch. The honest answer is that the
        // user has to reach for the control.
        app.auto_apply_intent = None;
        app.auto_apply_manual_only = None;
        app.clear_messages_for_test();
        app.surface_update_apply_outcome("automatic", failed(), false);
        assert_eq!(row(&app), stopped);

        // A LIVE INTENT for this artifact: a wake is already folded into the event
        // loop — nothing to say.
        app.auto_apply_intent = Some(crate::AutoApplyIntent {
            build,
            dmg_sha256: [0xab; 32],
            retry_at: std::time::Instant::now() + std::time::Duration::from_secs(600),
            attempts: 0,
        });
        app.clear_messages_for_test();
        app.surface_update_apply_outcome("automatic", failed(), false);
        assert_eq!(row(&app), None);

        // A LEFTOVER intent for a SUPERSEDED build schedules nothing for the
        // artifact on screen, so it must not borrow the silence.
        app.auto_apply_intent = Some(crate::AutoApplyIntent {
            build: build + 7,
            dmg_sha256: [0xab; 32],
            retry_at: std::time::Instant::now() + std::time::Duration::from_secs(600),
            attempts: 0,
        });
        app.clear_messages_for_test();
        app.surface_update_apply_outcome("automatic", failed(), false);
        assert_eq!(row(&app), stopped);

        // A LATCH WITH A DEADLINE is the other carrier: the lane is standing down
        // but `about_to_wait` will lapse it, so it does come back on its own —
        // on every physical attempt, the first included.
        app.auto_apply_intent = None;
        let lapsing = crate::AutoApplyManualOnly {
            build,
            dmg_sha256: [0xab; 32],
            retry_at: Some(std::time::Instant::now() + std::time::Duration::from_secs(600)),
        };
        app.auto_apply_manual_only = Some(lapsing);
        for cycles in [1_u8, 2, 5, 8] {
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

        // A DEADLINE-LESS latch (the policy-mismatch fail-safe, or a converged
        // artifact) genuinely does not come back by itself: said once, with the
        // control.
        app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
            build,
            dmg_sha256: [0xab; 32],
            retry_at: None,
        });
        app.clear_messages_for_test();
        app.surface_update_apply_outcome("automatic", failed(), false);
        assert_eq!(row(&app), stopped);

        // AND A PERSON IS NEVER SILENCED BY THE AUTOMATIC LANE'S SCHEDULE.
        //
        // A person's returned handoff spends nothing (driven, not hand-set: the
        // real completion path's `(Some(attempt), None)` arm), so the posture —
        // and the flow row — would not move; whoever just asked for the update is
        // exactly who must be told it did not happen.
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
        // (The test ticket stages its own artifact, version and all.)
        let asked = app
            .native_updater_service
            .snapshot()
            .staged
            .as_ref()
            .map(|stage| stage.version.clone())
            .expect("the ticket's stage");
        // Ruling 143: the lane retries it by itself, so the person's own press
        // ends in its switch row's Fault echo and a RECORD in main's words —
        // never a second row for a retry nobody can hurry.
        assert_eq!(row(&app), None, "a self-retry takes no row");
        assert_eq!(
            app.update_record_text(),
            Some(format!(
                "Couldn't install aterm v{asked} \u{2014} will try again by itself"
            )),
            "whoever just asked for the update finds it did not happen, on record"
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
        app.auto_apply_manual_only = Some(lapsing);
        app.clear_messages_for_test();
        app.surface_update_apply_outcome("automatic", failed(), false);
        assert_eq!(
            row(&app),
            Some(format!(
                "Couldn't install aterm v{asked} \u{2014} {}",
                crate::update_words::INSTALL_FROM_MENU
            ))
        );
    }

    /// THE FLOW SAYS WHAT HAPPENS TO THE PERSON'S WORK ONCE (2026-09-23). "Your
    /// shells keep running" was on four rows of one update — the staged row, the
    /// installing row, the finishing row, the landing — and a row that lasts under
    /// a second cannot even be read. Walked through every phase on the band, the
    /// one sentence about the person's typing appears exactly once (the switch,
    /// where keys are queued), the title is one from the first byte to the switch,
    /// and the landing claims nothing.
    #[test]
    fn the_flow_says_what_happens_to_the_persons_work_once() {
        use crate::messages_host::FlowPhase;
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = App::headless_for_test();
        let mut said: Vec<String> = Vec::new();
        let look = |app: &App, said: &mut Vec<String>| {
            said.push(app.update_row_text().expect("the flow row is up"));
        };
        app.note_update_progress(&aterm_update::Progress::Downloading {
            version: "0.79.0".into(),
            bytes_done: 1,
            bytes_total: 2,
        });
        look(&app, &mut said);
        app.note_update_progress(&aterm_update::Progress::Verifying {
            version: "0.79.0".into(),
        });
        look(&app, &mut said);
        // A windowed App's automatic posture (a headless one has no seamless
        // lane), then the ladder's keys-only phase.
        app.post_update_row(
            crate::update_words::staged(
                "0.79.0",
                7,
                Some(crate::update_words::ApplyPosture::Automatic),
            ),
            "0.79.0",
            FlowPhase::Staged {
                build: 7,
                flow: true,
            },
        );
        look(&app, &mut said);
        assert!(app.restate_update_flow_holds(7, crate::update_words::Holds::Typing));
        look(&app, &mut said);
        assert!(app.restate_update_flow(
            crate::messages_host::restatement_of(&crate::update_words::installing("0.79.0")),
            Some(FlowPhase::Installing),
        ));
        look(&app, &mut said);
        assert!(app.restate_update_flow(
            crate::messages_host::restatement_of(&crate::update_words::finishing("0.79.0")),
            Some(FlowPhase::Finishing),
        ));
        look(&app, &mut said);
        app.post_update_landed("0.79.0", 7);
        // The landing is the flow row's Complete echo and a record (ruling 141).
        assert_eq!(app.update_row_text(), None, "the landing takes no row");
        assert!(
            app.messages
                .log()
                .records()
                .any(|r| r.title == "Updated to aterm v0.79.0" && r.detail.is_empty()),
            "the landing's record claims nothing"
        );
        let mentions = said
            .iter()
            .filter(|line| line.contains("shells") || line.contains("what you type"))
            .count();
        assert_eq!(mentions, 1, "{said:#?}");
        // A title per phase, verb first (ruling 143), one row throughout.
        let titles: Vec<&str> = said
            .iter()
            .map(|line| line.split(" \u{2014} ").next().unwrap_or_default())
            .collect();
        assert_eq!(
            titles,
            [
                "Downloading aterm v0.79.0",
                "Checking aterm v0.79.0",
                "Installing aterm v0.79.0",
                "Installing aterm v0.79.0",
                "Installing aterm v0.79.0",
                "Finishing aterm v0.79.0",
            ],
            "{said:#?}"
        );
    }

    /// ONLY WHAT A PERSON CAN ACT ON IS NAMED (2026-09-23): a close preflight that
    /// refuses the install for unsaved editor or Settings work puts that one
    /// sentence on the row, whatever the schedule; a blocker that clears by
    /// itself (a restore landing, a checkpoint running) posts nothing while the
    /// lane will try again, and a stopped lane points at the Version menu.
    #[test]
    fn only_a_blocker_a_person_can_clear_is_named_on_the_row() {
        let mut app = App::headless_for_test();
        let blocked = |reasons: &[&str]| UpdateOutcome::Blocked {
            reasons: reasons.iter().map(|r| (*r).to_string()).collect(),
        };
        for reason in [
            App::UNSAVED_NATIVE_WORK_BLOCKS_APPLY,
            "Review Settings Drafts: 1 Settings view(s) have unsaved text",
            "Checkpoint Drafts: 2 document(s) have uncheckpointed edits",
            "Retry: 1 document checkpoint(s) previously failed",
        ] {
            app.clear_messages_for_test();
            app.react_to_update_apply_outcome("automatic", blocked(&[reason]), false);
            assert_eq!(
                app.update_row_text(),
                Some(format!(
                    "{UPDATE_WAITS_FOR_YOU} \u{2014} {}",
                    App::UNSAVED_NATIVE_WORK_BLOCKS_APPLY
                )),
                "{reason}"
            );
        }
        // Clears by itself, and nothing is scheduled here (no stage): the lane
        // has stopped, and the menu is named — on record, since with no build
        // staged there is no `Install now` to press (ruling 143).
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
        // …and while a retry IS scheduled, nothing at all.
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

    /// THE BLOCKED LANE'S RESTATEMENT puts the editor words only where the lane
    /// installs by itself (`App::restate_update_flow_holds` carries the positive
    /// case). It re-states the posture FIRST: a headless App has no seamless lane,
    /// so the automatic flow row a check painted becomes the ready row that says
    /// how this process installs — and the words about the editor never land on
    /// it, because nothing here is on its way in.
    #[test]
    fn a_blocked_restatement_names_the_editor_only_on_a_row_the_lane_installs() {
        let mut app = App::headless_for_test();
        let build = stage_one_build(&mut app);
        app.post_update_row(
            crate::update_words::staged(
                &format!("1.0.{build}"),
                build,
                Some(crate::update_words::ApplyPosture::Automatic),
            ),
            &format!("1.0.{build}"),
            crate::messages_host::FlowPhase::Staged { build, flow: true },
        );
        app.restate_blocked_update_bar(build, &[App::UNSAVED_NATIVE_WORK_BLOCKS_APPLY.to_string()]);
        // Headless: the posture's words are a RECORD (ruling 143) — the flow
        // row folds into it, and the editor words land nowhere.
        assert!(
            app.messages
                .live_by_key(crate::update_words::KEY_PROGRESS)
                .is_none(),
            "headless: no flow row stands"
        );
        let record = app
            .messages
            .log()
            .records()
            .rev()
            .find(|r| r.key.as_deref() == Some(crate::update_words::KEY_PROGRESS))
            .expect("the staged record");
        assert_eq!(
            record.detail,
            vec![crate::update_words::staged_detail(
                crate::update_words::ApplyPosture::HandoffDisabled {
                    why: crate::app_update_handoff::HandoffUnavailable::Headless,
                    veto: None,
                }
            )]
        );
        assert_ne!(record.detail[0], crate::update_words::EDITOR_HOLDS_IT);
        assert_eq!(
            app.update_flow_row_for(build),
            None,
            "the ready row is not the flow"
        );
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
                message: "activating in place".to_string(),
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

    /// The staged DECISION is raised once (design §10.5 H2, ruling 119), and
    /// only under a posture where a press is how the build installs.
    #[test]
    fn a_stopped_lane_raises_the_staged_decision_once() {
        use crate::update_words::ApplyPosture;
        let mut app = App::headless_for_test();
        let build = stage_one_build(&mut app);
        // No staged build of that number: nothing to offer.
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
        assert_eq!(row.msg.actions[0].label(), "Install now");
        // The person lets it lapse (its hold ran out). Every later reconcile
        // re-states the same posture: the question stays off the glass — the
        // Version menu and Software Update still offer it (review 2026-09-24).
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
        // A posture that has just become a decision (the lane stopped under
        // another one) raises it once more.
        app.ensure_staged_decision_in(build, ApplyPosture::ManualByConfig);
        assert!(
            app.staged_update_row().is_some(),
            "a new posture asks again"
        );
    }

    /// THE PERSON'S PRESS AGAINST THE LANE (ruling 143): a failure the lane
    /// retries by itself is a RECORD in main's words, whoever pressed — the
    /// person's switch row ends in the Fault echo; a lane that STOPPED is a
    /// decision row whose `Install now` is the Version menu's press.
    #[test]
    fn a_retried_failure_is_a_record_and_a_stopped_lane_a_decision() {
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = App::headless_for_test();
        let build = stage_one_build(&mut app);
        let failed = || UpdateOutcome::Failed {
            message: "overlap handoff failed safely: handoff proof ended TimedOut".to_string(),
        };
        app.clear_messages_for_test();
        app.react_to_update_apply_outcome("automatic handoff", failed(), false);
        let row = app
            .update_row_text()
            .expect("with nothing retrying, the stopped lane is a row");
        assert!(row.starts_with("Couldn't install aterm v"), "{row}");
        let live = app
            .messages
            .live_by_key(crate::update_words::KEY_OUTCOME)
            .expect("the outcome row");
        assert_eq!(
            live.msg.actions,
            vec![aterm_messages::Intent::ApplyUpdate { build }],
            "the press is on the glass"
        );
        assert_eq!(app.message_band_rows, 1);
    }

    /// ONE DECISION PER STOPPED LANE (rulings 119 and 143; review
    /// 2026-09-24). A stand-down's posture restatement turns the flow row
    /// into the ready row (`aterm vX is ready`, `Install now`); the stopped
    /// lane's outcome that follows carries the same press. The outcome is the
    /// one row: the ready row is withdrawn under it — on the Failed path and
    /// on the Blocked one.
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
    /// NO ✓ BESIDE A FAILURE (ruling 159). The automatic lane's staged row is
    /// the busy `Installing aterm vX`; resolved `Ok` its Complete echo says
    /// `✓ Installed aterm vX` (ruling 154). That is true once the bytes are
    /// installed (`InstalledNeedsRelaunch`) and false when the artifact is
    /// gone (`Failed` with no staged build): there the row is withdrawn and
    /// only `Update didn't finish` speaks.
    #[test]
    fn a_gone_artifact_never_echoes_installed_beside_its_failure() {
        use aterm_messages::EchoKind;
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let build = 41;
        for (outcome, echo) in [
            (
                UpdateOutcome::InstalledNeedsRelaunch {
                    build,
                    message: "activation pending".to_string(),
                },
                Some(EchoKind::Complete),
            ),
            (
                UpdateOutcome::Failed {
                    message: "the staged artifact was retired".to_string(),
                },
                None,
            ),
        ] {
            let mut app = App::headless_for_test();
            assert!(
                app.native_updater_service.snapshot().staged.is_none(),
                "PRECONDITION: no staged build"
            );
            app.post_update_row(
                crate::update_words::staged(
                    "9.9.9",
                    build,
                    Some(crate::update_words::ApplyPosture::Automatic),
                ),
                "9.9.9",
                crate::messages_host::FlowPhase::Staged { build, flow: true },
            );
            assert!(
                app.staged_update_row().is_some(),
                "PRECONDITION: staged row"
            );
            app.react_to_update_apply_outcome("automatic handoff", outcome.clone(), false);
            assert_eq!(app.staged_update_row(), None, "{outcome:?}: the row left");
            let complete = app
                .messages
                .echoes()
                .iter()
                .find(|e| e.kind == EchoKind::Complete)
                .map(|e| e.kind);
            assert_eq!(complete, echo, "{outcome:?}");
        }
    }
}
