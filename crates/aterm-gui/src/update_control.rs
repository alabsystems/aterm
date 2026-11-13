// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Memory-only GUI facts for updater control requests and staged installed-bundle activation.

use crate::native_update_auto_intent::ApplyPhase;
use crate::native_updater_service::{StagedUpdate, UpdaterPhase};
use crate::update_words::ApplyPosture;
use crate::{App, Wake};
use aterm_types::MutexExt as _;

pub(crate) struct Snapshot {
    /// Live automatic-checking preference, independent of platform support and install policy.
    pub(crate) enabled: bool,
    pub(crate) owner: Option<String>,
    pub(crate) repo: Option<String>,
    /// Live automatic-install preference, including when no update is staged.
    pub(crate) auto_apply: bool,
    staged: Option<StagedUpdate>,
    posture: Option<ApplyPosture>,
    /// Where the automatic lane stands on its ladder, for the staged build.
    phase: Option<ApplyPhase>,
    /// The restored agents' hold is holding the automatic lane
    /// (`HostHandle::restored_pending`).
    restored_hold: bool,
    /// The hold the lane named for the staged build past its ladder
    /// (`App::auto_apply_hold_token`).
    lane_hold: Option<&'static str>,
    /// The accepted apply is waiting for its account-work epoch to retire.
    account_work_hold: bool,
    applying: bool,
    retry_scheduled: bool,
}

impl Snapshot {
    pub(crate) fn capture(app: &App) -> Self {
        Self::capture_with(app, crate::app_update_screen::ApplyPostureEnv::observe(app))
    }

    /// [`Self::capture`] under the posture inputs `env` (the live ones, or a test's).
    pub(crate) fn capture_with(app: &App, env: crate::app_update_screen::ApplyPostureEnv) -> Self {
        let updater = app.native_updater_service.snapshot();
        let now = std::time::Instant::now();
        Self {
            enabled: app
                .config
                .update
                .as_ref()
                .and_then(|u| u.enabled)
                .unwrap_or(true),
            owner: app.config.update.as_ref().and_then(|u| u.owner.clone()),
            repo: app.config.update.as_ref().and_then(|u| u.repo.clone()),
            auto_apply: app
                .config
                .update
                .as_ref()
                .and_then(|u| u.auto_apply)
                .unwrap_or(true),
            staged: updater.staged.clone(),
            posture: updater
                .staged
                .as_ref()
                .map(|s| app.apply_posture_with(s.build, env)),
            phase: updater
                .staged
                .as_ref()
                .map(|_| app.automatic_apply_phase(now)),
            restored_hold: app
                .harness
                .as_ref()
                .is_some_and(|host| host.restored_pending(now)),
            lane_hold: updater
                .staged
                .as_ref()
                .and_then(|s| app.auto_apply_hold_token(s.build)),
            account_work_hold: app.account_drain_pending(),
            applying: updater.phase == UpdaterPhase::Applying,
            retry_scheduled: updater
                .staged
                .as_ref()
                .is_some_and(|s| app.automatic_apply_retry_scheduled(s.build)),
        }
    }

    /// The reducer's stage as [`Self::capture`] read it.
    pub(crate) const fn staged(&self) -> Option<&StagedUpdate> {
        self.staged.as_ref()
    }

    /// Automatic lane ladder phase for the staged build (`apply_phase=` on the wire).
    pub(crate) fn apply_phase(&self, status: &aterm_update::UpdateStatus) -> Option<&'static str> {
        match self.apply_posture(status) {
            "automatic" | "automatic-idle" => self.phase.map(ApplyPhase::as_str),
            _ => None,
        }
    }

    /// What holds this apply (`apply_held=` on the wire), for the same staged artifact: an accepted
    /// apply's account work; under an automatic posture the restored agents' hold while it holds,
    /// else the hold the lane last named past its ladder and has not attempted past
    /// (`native_update_lane_hold::LaneHold::token`).
    pub(crate) fn apply_held(&self, status: &aterm_update::UpdateStatus) -> Option<&'static str> {
        match self.apply_posture(status) {
            "applying" if self.account_work_hold => Some("account-work"),
            "automatic" | "automatic-idle" if self.restored_hold => {
                Some(crate::native_update_lane_hold::LaneHold::RestoredAgents.token())
            }
            "automatic" | "automatic-idle" => self.lane_hold,
            _ => None,
        }
    }

    /// The ledger and the GUI can observe different generations.
    pub(crate) fn apply_posture(&self, status: &aterm_update::UpdateStatus) -> &'static str {
        if !status
            .staged_build
            .is_some_and(|b| b > status.current_build)
        {
            return if status.enabled { "none" } else { "disabled" };
        }
        // A stage can still install when automatic checking is off.
        let matched =
            self.staged
                .as_ref()
                .is_some_and(|stage| match status.staged_dmg_sha256.as_deref() {
                    Some(_) => crate::native_updater_service::durable_artifact_identity_matches(
                        status.staged_build,
                        status.staged_commit.as_deref(),
                        status.staged_dmg_sha256.as_deref(),
                        stage.build,
                        stage.commit.as_deref(),
                        &stage.dmg_sha256,
                    ),
                    None => {
                        Some(stage.build) == status.staged_build
                            && stage.is_installed_activation()
                            && status
                                .staged_commit
                                .as_deref()
                                .zip(stage.commit.as_deref())
                                .is_some_and(|(observed, expected)| {
                                    crate::native_updater_service::usable_commit_identity(observed)
                                        && observed.trim().eq_ignore_ascii_case(expected)
                                })
                    }
                });
        if !matched {
            return "unreconciled";
        }
        if self.applying {
            return "applying";
        }
        match self.posture {
            Some(ApplyPosture::Automatic) if self.retry_scheduled => "automatic",
            Some(ApplyPosture::Automatic) => "automatic-idle",
            Some(ApplyPosture::AtNextLaunch) => "next-launch",
            Some(ApplyPosture::ManualByConfig) => "manual-config",
            Some(ApplyPosture::VetoedByEnv { .. }) => "disabled-env",
            Some(ApplyPosture::ManualOnlyLatched { lapses: true }) => "retry-wait",
            Some(ApplyPosture::ManualOnlyLatched { lapses: false } | ApplyPosture::TrialSpent) => {
                "manual-only"
            }
            Some(ApplyPosture::HandoffDisabled { .. }) => "handoff-unavailable",
            None => "unknown",
        }
    }

    pub(crate) fn apply_policy_reason(
        &self,
        status: &aterm_update::UpdateStatus,
    ) -> Option<&'static str> {
        match self.apply_posture(status) {
            "manual-config" => Some("update.auto_apply=false"),
            "disabled-env" => match self.posture {
                Some(ApplyPosture::VetoedByEnv { var }) => Some(var),
                _ => None,
            },
            "handoff-unavailable" => match self.posture {
                Some(ApplyPosture::HandoffDisabled { why, .. }) => Some(why.cause()),
                _ => None,
            },
            _ => None,
        }
    }
}

/// The worker reads source and policy together.
pub(crate) fn check_settings_provider(
    proxy: aterm_window::event_loop::EventLoopProxy<Wake>,
) -> aterm_update::CheckSettingsProvider {
    let proxy = std::sync::Mutex::new(proxy);
    std::sync::Arc::new(move || {
        let proxy = proxy.lock().ok()?;
        let live = crate::control::control_media::call_main_within(
            &proxy,
            std::time::Duration::from_secs(2),
            |reply| Wake::ReadUpdateControl { reply },
        )
        .ok()?;
        let settings = aterm_update::CheckSettings {
            source: aterm_update::Source::resolve(live.owner.as_deref(), live.repo.as_deref()),
            auto_apply: live.auto_apply,
        };
        #[cfg(target_os = "linux")]
        return admit_check_settings(settings, crate::app_config::update_check_settings_setting());
        #[cfg(not(target_os = "linux"))]
        Some(settings)
    })
}

/// Disk admission is worker-only. Startup/reload may retain a usable GUI with
/// fallback defaults after a parse failure; those defaults are not permission
/// to replace its executable. Both live and persisted policy must admit apply.
#[cfg(any(target_os = "linux", test))]
fn admit_check_settings(
    mut live: aterm_update::CheckSettings,
    persisted: Option<aterm_update::CheckSettings>,
) -> Option<aterm_update::CheckSettings> {
    let persisted = persisted?;
    if live.source != persisted.source {
        return None;
    }
    live.auto_apply &= persisted.auto_apply;
    Some(live)
}

/// Every completion refreshes the GUI, including errors, retirement, and installed activation.
pub(crate) fn announce_stage(status: &aterm_update::UpdateStatus, send: impl FnOnce(Wake)) {
    if status.enabled
        && let Some(build) = status.staged_build.filter(|b| *b > status.current_build)
    {
        send(Wake::UpdateStaged {
            build,
            version: status
                .staged_version
                .clone()
                .unwrap_or_else(|| format!("build {build}")),
        });
    } else {
        send(Wake::UpdateCheckCompleted);
    }
}

static STAGED_ACTIVATION: std::sync::Mutex<Option<StagedUpdate>> = std::sync::Mutex::new(None);

/// Publish the reducer's stage for an installed-bundle activation, or clear the record.
pub(crate) fn publish_staged_activation(staged: Option<&StagedUpdate>) {
    *STAGED_ACTIVATION.lock_or_recover() = activation_to_publish(staged);
}

fn activation_to_publish(staged: Option<&StagedUpdate>) -> Option<StagedUpdate> {
    staged
        .filter(|stage| stage.is_installed_activation())
        .cloned()
}

/// The activation [`publish_staged_activation`] last published.
pub(crate) fn published_activation() -> Option<StagedUpdate> {
    STAGED_ACTIVATION.lock_or_recover().clone()
}

/// The reducer's stage `update status` folds.
pub(crate) fn status_activation<'a>(
    live: Option<&'a Snapshot>,
    published: Option<&'a StagedUpdate>,
) -> Option<&'a StagedUpdate> {
    live.map_or(published, Snapshot::staged)
}

/// Windows: the build staged on this machine (`aterm_update::windows`), as this window
/// announces it. Memory only: the pointer file and the record beside the executable are the
/// durable facts, read again at every look.
#[cfg(windows)]
struct WindowsStaged {
    /// The staged build the strip label and the palette row offer to install.
    shown: Option<(u64, String)>,
    /// An installer this window started is running.
    installing: bool,
    /// How that installer ended, until the main thread reads it.
    finished: Option<Result<aterm_update::windows::Applied, String>>,
    /// The build this window has already said is installed (its own installer's success, or
    /// the restart note for one laid under it): said once, and not offered here again.
    told_installed: Option<u64>,
}

#[cfg(windows)]
static WINDOWS_STAGED: std::sync::Mutex<WindowsStaged> = std::sync::Mutex::new(WindowsStaged {
    shown: None,
    installing: false,
    finished: None,
    told_installed: None,
});

/// The running image, as the staged-build status names it.
#[cfg(windows)]
fn windows_running() -> aterm_update::windows::Running<'static> {
    aterm_update::windows::Running {
        version: crate::build_info::version(),
        commit: crate::build_info::git_commit(),
        build_number: crate::running_build_number(),
    }
}

/// What a notice leaves on the window: the build its install row offers, and the build whose
/// restart note is due. A build laid under the window (by the build loop, a shell, another
/// window or this one) takes the row down and is said once; a withdrawn offer takes the row
/// down and says nothing.
#[cfg(windows)]
fn windows_mark_after(
    told_installed: Option<u64>,
    notice: &aterm_update::windows::Notice,
) -> (
    Option<(u64, String)>,
    Option<&aterm_update::windows::StagedBuild>,
) {
    use aterm_update::windows::Notice;
    match notice {
        Notice::Nothing => (None, None),
        // A dist that lays no record leaves the build "offered" after this window installed
        // it: what this window installed is not offered here again.
        Notice::Offered(staged) if told_installed == Some(staged.build_number) => (None, None),
        Notice::Offered(staged) => (Some((staged.build_number, staged.version.clone())), None),
        Notice::Installed(staged) => (
            None,
            (told_installed != Some(staged.build_number)).then_some(staged),
        ),
    }
}

/// Start the staged-build watch: two small file reads at launch and every
/// `aterm_update::windows::WATCH_EVERY` on its own thread, and a wake only when what the
/// window should say changes.
#[cfg(windows)]
pub(crate) fn spawn_windows_staged_watch(proxy: aterm_window::event_loop::EventLoopProxy<Wake>) {
    let watching = aterm_update::windows::spawn_watch(
        windows_running(),
        Box::new(move |notice| {
            let _ = proxy.send_event(Wake::StagedBuildNotice(notice));
        }),
    );
    if !watching {
        aterm_log::info!("update: no staged-build watch (no %LOCALAPPDATA%, or no thread)");
    }
}

/// The staged build this window offers, for a republish of the update state.
#[cfg(windows)]
pub(crate) fn windows_staged_shown() -> Option<(u64, String)> {
    WINDOWS_STAGED.lock_or_recover().shown.clone()
}

/// `update status` on Windows: the running build against the staged and the installed one.
#[cfg(windows)]
pub(crate) fn windows_status_line() -> String {
    let running = windows_running();
    format!(
        "{}\n",
        aterm_update::windows::control_line(running, &aterm_update::windows::here(running))
    )
}

/// `update apply` on Windows, answered before the window runs the installer: what was asked
/// for, or why there is nothing to install.
#[cfg(windows)]
pub(crate) fn windows_apply_reply() -> String {
    let build = crate::running_build_number();
    let here = aterm_update::windows::here(windows_running());
    match here.installable() {
        Ok(staged)
            if WINDOWS_STAGED.lock_or_recover().told_installed == Some(staged.build_number) =>
        {
            format!(
                "ERR build {} ({}) is already installed; restart aterm to use it\n",
                staged.build_number, staged.version
            )
        }
        Ok(staged) => format!(
            "OK apply requested: build {} ({}) over build {build}; the installer stops nothing, \
             and aterm.log and Messages carry its verdict\n",
            staged.build_number, staged.version
        ),
        Err(why) => format!("ERR {why}\n"),
    }
}

#[cfg(windows)]
impl App {
    /// The staged-build watch, or an installer this window ran, reports what the window should
    /// now say: report the installer's verdict, then put up or take down the install row.
    pub(crate) fn windows_staged_notice(&mut self, notice: &aterm_update::windows::Notice) {
        let finished = WINDOWS_STAGED.lock_or_recover().finished.take();
        match finished {
            Some(Ok(applied)) => {
                WINDOWS_STAGED.lock_or_recover().told_installed = Some(applied.staged.build_number);
                self.post_message(
                    aterm_messages::Message::new(
                        aterm_messages::tags::UPDATE,
                        aterm_messages::Severity::Success,
                        format!("Installed {}", applied.staged.name()),
                    )
                    .line("new windows and shells use it; this window keeps running the old build")
                    .line("restart aterm to move this window onto it")
                    .key("update-staged-install"),
                );
            }
            Some(Err(why)) => {
                self.post_message(
                    aterm_messages::Message::new(
                        aterm_messages::tags::UPDATE,
                        aterm_messages::Severity::Warn,
                        "The staged build did not install",
                    )
                    .sentence(why)
                    .line("`aterm update apply` shows why")
                    .key("update-staged-install"),
                );
            }
            None => {}
        }
        self.windows_follow_notice(notice);
    }

    /// Put up or take down the install row for `notice`, and say once that a build was laid
    /// under this window. `true` when that was said now.
    fn windows_follow_notice(&mut self, notice: &aterm_update::windows::Notice) -> bool {
        let (shown, restart) = {
            let mut state = WINDOWS_STAGED.lock_or_recover();
            let (shown, restart) = windows_mark_after(state.told_installed, notice);
            state.shown.clone_from(&shown);
            if let Some(staged) = restart {
                state.told_installed = Some(staged.build_number);
            }
            (shown, restart)
        };
        if let Some(staged) = restart {
            self.post_message(
                aterm_messages::Message::new(
                    aterm_messages::tags::UPDATE,
                    aterm_messages::Severity::Info,
                    format!("{} is installed", staged.name()),
                )
                .line("new windows and shells use it; this window keeps running the old build")
                .line("restart aterm to move this window onto it")
                .key("update-staged-install"),
            );
        }
        self.windows_set_staged_mark(shown);
        restart.is_some()
    }

    /// Put up the install row and the strip's mark for `shown`, or take them down.
    fn windows_set_staged_mark(&mut self, shown: Option<(u64, String)>) {
        self.relaunch =
            shown.map(|(build, version)| crate::relaunch_notice::RelaunchNotice { build, version });
        self.refresh_version_menu();
        self.palette_refresh_live();
        self.request_redraw_all_windows();
    }

    /// The Apply row, the strip's update mark and `update apply` on Windows: install the staged
    /// build on a worker thread, stopping nothing. A row this window still shows for a build
    /// that is no longer offered (the pointer moved on, the dist went, or the build was laid
    /// under the window since the last look) follows this look and says why nothing installs;
    /// it never falls through to the network updater's page. `false` when this window offered
    /// nothing and nothing is offered (the caller's own answer stands).
    pub(crate) fn windows_apply_staged_update(&mut self) -> bool {
        let running = windows_running();
        let here = aterm_update::windows::here(running);
        let staged = match here.installable() {
            Ok(staged) => staged.clone(),
            Err(why) => {
                if WINDOWS_STAGED.lock_or_recover().shown.is_none() {
                    return false;
                }
                // Said already when the build was laid under the window: the restart note.
                if !self.windows_follow_notice(&here.notice()) {
                    self.post_message(
                        aterm_messages::Message::new(
                            aterm_messages::tags::UPDATE,
                            aterm_messages::Severity::Info,
                            "Nothing to install",
                        )
                        .sentence(why)
                        .key("update-staged-install"),
                    );
                }
                return true;
            }
        };
        let Some(proxy) = self.proxy.clone() else {
            return false;
        };
        let busy = {
            let mut state = WINDOWS_STAGED.lock_or_recover();
            if state.installing {
                Some("The staged build is still installing")
            } else if state.told_installed == Some(staged.build_number) {
                Some("The staged build is already installed \u{2014} restart aterm to use it")
            } else {
                state.installing = true;
                None
            }
        };
        if let Some(said) = busy {
            self.post_message(
                aterm_messages::Message::new(
                    aterm_messages::tags::UPDATE,
                    aterm_messages::Severity::Info,
                    said,
                )
                .key("update-staged-install"),
            );
            return true;
        }
        let standing = here.standing;
        let worker = std::thread::Builder::new()
            .name("aterm-staged-install".into())
            .spawn(move || {
                let result = std::env::current_exe()
                    .map_err(|error| format!("cannot locate this executable ({error})"))
                    .and_then(|exe| {
                        aterm_update::windows::apply(
                            &standing,
                            &exe,
                            aterm_update::windows::ApplyMode::Staged,
                            aterm_update::windows::InstallerOutput::Capture,
                        )
                    });
                {
                    let mut state = WINDOWS_STAGED.lock_or_recover();
                    state.installing = false;
                    state.finished = Some(result);
                }
                // Look again: the installer laid the build's record, or it did not install.
                let notice = aterm_update::windows::here(running).notice();
                let _ = proxy.send_event(Wake::StagedBuildNotice(notice));
            });
        if let Err(error) = worker {
            WINDOWS_STAGED.lock_or_recover().installing = false;
            aterm_log::warn!("update: could not start the staged install: {error}");
            return false;
        }
        aterm_log::info!("update: installing staged {}", staged.name());
        true
    }
}

#[cfg(test)]
#[path = "update_control_tests.rs"]
mod tests;
