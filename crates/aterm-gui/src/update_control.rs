// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Memory-only GUI facts for updater control requests and staged installed-bundle activation.

use crate::native_update_auto_intent::ApplyPhase;
use crate::native_updater_service::{StagedUpdate, UpdaterPhase};
use crate::update_words::ApplyPosture;
use crate::{App, Wake};
use aterm_types::MutexExt as _;

pub(crate) struct Snapshot {
    pub(crate) owner: Option<String>,
    pub(crate) repo: Option<String>,
    pub(crate) auto_apply: bool,
    staged: Option<StagedUpdate>,
    posture: Option<ApplyPosture>,
    /// Where the automatic lane stands on its ladder, for the staged build.
    phase: Option<ApplyPhase>,
    /// The restored agents' hold is holding the automatic lane
    /// (`HostHandle::restored_pending`).
    restored_hold: bool,
    applying: bool,
    retry_scheduled: bool,
}

impl Snapshot {
    pub(crate) fn capture(app: &App) -> Self {
        let updater = app.native_updater_service.snapshot();
        let now = std::time::Instant::now();
        Self {
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
                .map(|s| app.apply_posture_for(s.build)),
            phase: updater
                .staged
                .as_ref()
                .map(|_| app.automatic_apply_phase(now)),
            restored_hold: app
                .harness
                .as_ref()
                .is_some_and(|host| host.restored_pending(now)),
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

    /// What holds the automatic lane past its ladder (`apply_held=` on the wire).
    pub(crate) fn apply_held(&self, status: &aterm_update::UpdateStatus) -> Option<&'static str> {
        (matches!(self.apply_posture(status), "automatic" | "automatic-idle") && self.restored_hold)
            .then_some("restored-agents")
    }

    /// The ledger and the GUI can observe different generations.
    pub(crate) fn apply_posture(&self, status: &aterm_update::UpdateStatus) -> &'static str {
        if !status.enabled {
            return "disabled";
        }
        if !status
            .staged_build
            .is_some_and(|b| b > status.current_build)
        {
            return "none";
        }
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
            Some(ApplyPosture::WaitsForTerminal) => "waits-terminal",
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

#[cfg(test)]
mod tests {
    use super::*;

    fn status() -> aterm_update::UpdateStatus {
        aterm_update::UpdateStatus {
            enabled: true,
            installable: true,
            current_build: 10,
            staged_build: Some(11),
            staged_version: Some("test".into()),
            staged_commit: Some("a".repeat(40)),
            staged_dmg_sha256: Some("b".repeat(64)),
            changelog: None,
            outcome: String::new(),
            updated_at: String::new(),
            failing_checks: 0,
            failing_kind: String::new(),
            failing_applies: 0,
            failing_since: String::new(),
            failing_persistent: false,
            failing_checks_kind: String::new(),
            channel_unreadable: false,
            linux: None,
        }
    }

    #[test]
    fn automatic_check_requires_both_live_and_readable_persisted_policy() {
        let settings = |auto_apply| aterm_update::CheckSettings {
            source: aterm_update::Source {
                owner: "owner".into(),
                repo: "repo".into(),
            },
            auto_apply,
        };
        assert!(admit_check_settings(settings(true), None).is_none());
        for live in [false, true] {
            for disk in [false, true] {
                assert_eq!(
                    admit_check_settings(settings(live), Some(settings(disk)))
                        .unwrap()
                        .auto_apply,
                    live && disk
                );
            }
        }
        let mut repointed = settings(true);
        repointed.source.repo = "changed".into();
        assert!(admit_check_settings(settings(true), Some(repointed)).is_none());
    }

    #[test]
    fn control_check_announces_every_completion_without_inventing_a_stage() {
        let mut status = status();
        let mut sent = false;
        announce_stage(&status, |wake| {
            assert!(matches!(wake, Wake::UpdateStaged { build: 11, .. }));
            sent = true;
        });
        assert!(sent, "a manual socket check must wake the GUI reducer");
        status.enabled = false;
        announce_stage(&status, |wake| {
            assert!(matches!(wake, Wake::UpdateCheckCompleted))
        });
        status.enabled = true;
        status.staged_build = Some(10);
        announce_stage(&status, |wake| {
            assert!(matches!(wake, Wake::UpdateCheckCompleted))
        });
        status.staged_build = None;
        announce_stage(&status, |wake| {
            assert!(matches!(wake, Wake::UpdateCheckCompleted))
        });
    }

    #[test]
    fn control_posture_requires_the_same_artifact_and_a_real_retry() {
        let mut status = status();
        let mut snapshot = Snapshot {
            owner: None,
            repo: None,
            auto_apply: true,
            staged: Some(StagedUpdate {
                build: 11,
                version: "test".into(),
                commit: status.staged_commit.clone(),
                dmg_sha256: status.staged_dmg_sha256.clone().unwrap(),
                changelog: None,
                generation: 1,
            }),
            posture: Some(ApplyPosture::Automatic),
            phase: Some(ApplyPhase::PreferOutputGap),
            restored_hold: false,
            applying: false,
            retry_scheduled: false,
        };
        assert_eq!(snapshot.apply_posture(&status), "automatic-idle");
        assert_eq!(snapshot.apply_phase(&status), Some("prefer-output-gap"));
        snapshot.retry_scheduled = true;
        assert_eq!(snapshot.apply_posture(&status), "automatic");
        assert_eq!(snapshot.apply_phase(&status), Some("prefer-output-gap"));
        status.staged_commit = status.staged_commit.map(|s| s.to_uppercase());
        status.staged_dmg_sha256 = status.staged_dmg_sha256.map(|s| s.to_uppercase());
        assert_eq!(snapshot.apply_posture(&status), "automatic");
        snapshot.posture = Some(ApplyPosture::ManualByConfig);
        assert_eq!(snapshot.apply_posture(&status), "manual-config");
        assert_eq!(
            snapshot.apply_phase(&status),
            None,
            "no phase is promised for a lane that will not apply by itself"
        );
        assert_eq!(
            snapshot.apply_policy_reason(&status),
            Some("update.auto_apply=false")
        );
        status.staged_dmg_sha256 = Some("c".repeat(64));
        assert_eq!(snapshot.apply_posture(&status), "unreconciled");
        assert_eq!(snapshot.apply_policy_reason(&status), None);
        status.staged_dmg_sha256 = None;
        assert_eq!(snapshot.apply_posture(&status), "unreconciled");
        let stage = snapshot.staged.as_mut().unwrap();
        stage.dmg_sha256 = crate::native_updater_service::installed_activation_digest(
            stage.build,
            stage.commit.as_deref().unwrap(),
        );
        assert_eq!(snapshot.apply_posture(&status), "manual-config");
        status.staged_commit = Some("d".repeat(40));
        assert_eq!(snapshot.apply_posture(&status), "unreconciled");
    }

    #[test]
    fn the_restored_agents_hold_is_named_while_it_holds() {
        let status = status();
        let mut snapshot = Snapshot {
            owner: None,
            repo: None,
            auto_apply: true,
            staged: Some(StagedUpdate {
                build: 11,
                version: "test".into(),
                commit: status.staged_commit.clone(),
                dmg_sha256: status.staged_dmg_sha256.clone().unwrap(),
                changelog: None,
                generation: 1,
            }),
            posture: Some(ApplyPosture::Automatic),
            phase: Some(ApplyPhase::Land),
            restored_hold: true,
            applying: false,
            retry_scheduled: true,
        };
        assert_eq!(snapshot.apply_phase(&status), Some("land"));
        assert_eq!(snapshot.apply_held(&status), Some("restored-agents"));
        snapshot.restored_hold = false;
        assert_eq!(snapshot.apply_held(&status), None);
        snapshot.restored_hold = true;
        snapshot.posture = Some(ApplyPosture::ManualByConfig);
        assert_eq!(
            snapshot.apply_held(&status),
            None,
            "a lane that will not apply by itself is held by nothing"
        );

        let mut said = false;
        assert!(
            crate::native_update_auto_intent::restored_hold_line(11, true, &mut said).is_some(),
            "the hold's start is said"
        );
        assert_eq!(
            crate::native_update_auto_intent::restored_hold_line(11, true, &mut said),
            None,
            "once"
        );
        assert!(
            crate::native_update_auto_intent::restored_hold_line(11, false, &mut said).is_some(),
            "its end is said"
        );
        assert_eq!(
            crate::native_update_auto_intent::restored_hold_line(11, false, &mut said),
            None
        );
    }

    fn installed_activation(build: u64) -> StagedUpdate {
        let commit = "a".repeat(40);
        StagedUpdate {
            build,
            version: format!("build {build}"),
            dmg_sha256: crate::native_updater_service::installed_activation_digest(build, &commit),
            commit: Some(commit),
            changelog: None,
            generation: 1,
        }
    }

    #[test]
    fn only_an_activation_is_published() {
        let activation = installed_activation(12);
        assert_eq!(activation_to_publish(Some(&activation)), Some(activation));
        let download = StagedUpdate {
            dmg_sha256: "b".repeat(64),
            ..installed_activation(12)
        };
        assert_eq!(activation_to_publish(Some(&download)), None);
        assert_eq!(activation_to_publish(None), None);
    }

    #[test]
    fn the_live_stage_outranks_the_published_one() {
        let live = |staged| Snapshot {
            owner: None,
            repo: None,
            auto_apply: true,
            staged,
            posture: None,
            phase: None,
            restored_hold: false,
            applying: false,
            retry_scheduled: false,
        };
        let published = installed_activation(12);
        assert_eq!(status_activation(Some(&live(None)), Some(&published)), None);
        let current = live(Some(installed_activation(13)));
        assert_eq!(
            status_activation(Some(&current), Some(&published)),
            current.staged.as_ref()
        );
        assert_eq!(status_activation(None, Some(&published)), Some(&published));
    }

    #[test]
    fn control_source_is_the_live_gui_configuration() {
        let mut app = App::headless_for_test();
        let config = app.config.update.get_or_insert_with(Default::default);
        config.owner = Some("first-owner".into());
        config.repo = Some("first-repo".into());
        let first = Snapshot::capture(&app);
        let config = app.config.update.as_mut().unwrap();
        config.owner = Some("second-owner".into());
        config.repo = Some("second-repo".into());
        let second = Snapshot::capture(&app);
        assert_eq!(first.owner.as_deref(), Some("first-owner"));
        assert_eq!(first.repo.as_deref(), Some("first-repo"));
        assert_eq!(second.owner.as_deref(), Some("second-owner"));
        assert_eq!(second.repo.as_deref(), Some("second-repo"));
    }
}
