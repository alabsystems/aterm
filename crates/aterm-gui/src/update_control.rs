// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Memory-only GUI facts for updater control requests. Durable stage reads and
//! network work remain on the control worker; no ready marker is trusted here.

use crate::native_updater_service::{StagedUpdate, UpdaterPhase};
use crate::status_bars::ApplyPosture;
use crate::{App, Wake};

pub(crate) struct Snapshot {
    pub(crate) owner: Option<String>,
    pub(crate) repo: Option<String>,
    staged: Option<StagedUpdate>,
    posture: Option<ApplyPosture>,
    applying: bool,
    retry_scheduled: bool,
}

impl Snapshot {
    pub(crate) fn capture(app: &App) -> Self {
        let updater = app.native_updater_service.snapshot();
        Self {
            owner: app.config.update.as_ref().and_then(|u| u.owner.clone()),
            repo: app.config.update.as_ref().and_then(|u| u.repo.clone()),
            staged: updater.staged.clone(),
            posture: updater
                .staged
                .as_ref()
                .map(|s| app.apply_posture_for(s.build)),
            applying: updater.phase == UpdaterPhase::Applying,
            retry_scheduled: updater
                .staged
                .as_ref()
                .is_some_and(|s| app.automatic_apply_retry_scheduled(s.build)),
        }
    }

    /// The ledger and the GUI can observe different generations. Never attach a
    /// previous artifact's retry/permission to a replacement with the same build.
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
            Some(ApplyPosture::ManualByConfig) => "manual-config",
            Some(ApplyPosture::VetoedByEnv { .. }) => "disabled-env",
            Some(ApplyPosture::ManualOnlyLatched { lapses: true }) => "retry-wait",
            Some(ApplyPosture::ManualOnlyLatched { lapses: false }) => "manual-only",
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

/// Every completion refreshes the GUI, including errors, retirement and an
/// installed-only activation. A newer downloaded stage uses the background
/// check's existing hint. The reducer re-reads and validates durable facts before
/// arming anything; these notifications carry no apply authority.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn status() -> aterm_update::UpdateStatus {
        // No filesystem/global environment mutation: this value is the updater's
        // public wire input, with all unrelated fields at their test defaults.
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
            rescues: 0,
            failing_checks_kind: String::new(),
            channel_unreadable: false,
        }
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
            staged: Some(StagedUpdate {
                build: 11,
                version: "test".into(),
                commit: status.staged_commit.clone(),
                dmg_sha256: status.staged_dmg_sha256.clone().unwrap(),
                changelog: None,
                generation: 1,
            }),
            posture: Some(ApplyPosture::Automatic),
            applying: false,
            retry_scheduled: false,
        };
        assert_eq!(snapshot.apply_posture(&status), "automatic-idle");
        snapshot.retry_scheduled = true;
        assert_eq!(snapshot.apply_posture(&status), "automatic");
        status.staged_commit = status.staged_commit.map(|s| s.to_uppercase());
        status.staged_dmg_sha256 = status.staged_dmg_sha256.map(|s| s.to_uppercase());
        assert_eq!(snapshot.apply_posture(&status), "automatic");
        snapshot.posture = Some(ApplyPosture::ManualByConfig);
        assert_eq!(snapshot.apply_posture(&status), "manual-config");
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
